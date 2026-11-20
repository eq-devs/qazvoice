import 'dart:async';
import 'dart:ffi';
import 'dart:io';
import 'dart:isolate';
import 'dart:typed_data';

import 'package:ffi/ffi.dart';

import '../errors.dart';
import 'ffi.dart';

/// A background isolate that owns one native engine. Every model load,
/// inference call and file read runs there, never on the UI isolate.
/// Requests are served one at a time, in order; `cancel` is served at once.
class NativeWorker {
  NativeWorker._(this._iso, this._send, this._recv, this.kind);

  final Isolate _iso;
  final SendPort _send;
  final ReceivePort _recv;

  /// `tts`, `stt` or `punct`.
  final String kind;
  int _next = 0;
  bool _dead = false;
  final Map<int, _Pending> _pending = {};

  /// Starts a worker for [kind].
  static Future<NativeWorker> spawn(String kind) async {
    final recv = ReceivePort('qazvoice-$kind');
    final iso = await Isolate.spawn(
      _workerMain,
      [recv.sendPort, kind, QvLib.overridePath],
      debugName: 'qazvoice-$kind',
      errorsAreFatal: false,
    );
    final first = Completer<SendPort>();
    late NativeWorker w;
    recv.listen((msg) {
      if (!first.isCompleted) {
        first.complete(msg as SendPort);
        return;
      }
      w._onMessage(msg);
    });
    final errors = RawReceivePort((Object? e) => w._fail('$e'));
    iso.addErrorListener(errors.sendPort);
    final exit = RawReceivePort((Object? _) {
      errors.close();
      w._fail('worker exited');
    });
    iso.addOnExitListener(exit.sendPort);
    w = NativeWorker._(iso, await first.future, recv, kind);
    w._closers = [errors.close, exit.close];
    return w;
  }

  List<void Function()> _closers = const [];

  /// Number of requests not answered yet.
  int get pending => _pending.length;

  /// Sends [op] and returns its result. [onProgress] receives progress messages;
  /// [cancelToken] cancels the request (it then throws [QazCancelled]).
  Future<Object?> call(String op,
      [Object? args,
      void Function(Object? progress)? onProgress,
      QazCancelToken? cancelToken]) {
    if (_dead) return Future.error(const QazEngineError('engine is closed'));
    final id = _next++;
    final p = _Pending(onProgress);
    _pending[id] = p;
    void onCancel() {
      if (_pending.containsKey(id)) _send.send([id, '__cancel', id]);
    }

    cancelToken?.addListener(onCancel);
    _send.send([id, op, args]);
    return p.c.future.whenComplete(() => cancelToken?.removeListener(onCancel));
  }

  void _onMessage(Object? msg) {
    final m = msg as List;
    final id = m[0] as int;
    final kind = m[1] as int;
    final p = _pending[id];
    if (p == null) return;
    switch (kind) {
      case 0:
        _pending.remove(id);
        p.c.complete(m[2]);
      case 1:
        _pending.remove(id);
        p.c.completeError(errorFromWire(m[2]));
      default:
        p.onProgress?.call(m[2]);
    }
  }

  void _fail(String why) {
    if (_dead) return;
    _dead = true;
    for (final p in _pending.values) {
      p.c.completeError(QazEngineError('engine stopped: $why'));
    }
    _pending.clear();
    _recv.close();
    for (final c in _closers) {
      c();
    }
  }

  /// Frees the native engine and stops the isolate.
  Future<void> dispose() async {
    if (_dead) return;
    // Cancel whatever runs or waits, so the dispose request (which frees the
    // native engine) is served promptly instead of after a long job.
    for (final id in _pending.keys) {
      _send.send([id, '__cancel', id]);
    }
    try {
      await call('__dispose').timeout(const Duration(seconds: 30));
    } catch (_) {}
    _fail('disposed');
    _iso.kill(priority: Isolate.immediate);
  }
}

class _Pending {
  _Pending(this.onProgress);
  final Completer<Object?> c = Completer();
  final void Function(Object?)? onProgress;
}

/// Context handed to a request handler inside the worker.
class _Ctx {
  _Ctx(this.id, this._out, this._cancelled, this._disposing);
  final int id;
  final SendPort _out;
  final Set<int> _cancelled;
  final bool Function() _disposing;
  bool get cancelled => _cancelled.contains(id) || _disposing();
  void progress(Object? v) => _out.send([id, 2, v]);
  void checkCancelled() {
    if (cancelled) throw const QazCancelled();
  }
}

void _workerMain(List<Object?> init) {
  final out = init[0] as SendPort;
  final kind = init[1] as String;
  QvLib.overridePath = init[2] as String?;
  final handler = switch (kind) {
    'tts' => _TtsHandler(),
    'stt' => _SttHandler(),
    'punct' => _PunctHandler(),
    _ => throw ArgumentError(kind),
  };
  final cancelled = <int>{};
  var disposing = false;
  var chain = Future<void>.value();
  final port = RawReceivePort();
  port.handler = (Object? msg) {
    final m = msg as List;
    final id = m[0] as int;
    final op = m[1] as String;
    if (op == '__cancel') {
      cancelled.add(m[2] as int);
      return;
    }
    if (op == '__dispose') {
      // Everything queued before the dispose is cancelled.
      disposing = true;
    }
    chain = chain.then((_) async {
      final ctx = _Ctx(id, out, cancelled, () => disposing);
      try {
        if (op == '__dispose') {
          handler.dispose();
          out.send([id, 0, null]);
          port.close();
          return;
        }
        ctx.checkCancelled();
        final r = await handler.handle(op, m[2], ctx);
        out.send([id, 0, r]);
      } catch (e) {
        out.send(
            [id, 1, errorToWire(e is QazException ? e : QazEngineError('$e'))]);
      } finally {
        cancelled.remove(id);
      }
    });
  };
  out.send(port.sendPort);
}

abstract class _Handler {
  FutureOr<Object?> handle(String op, Object? args, _Ctx ctx);
  void dispose();
  QvLib get lib => QvLib.instance;
}

class _TtsHandler extends _Handler {
  Pointer<Void> _h = nullptr;

  @override
  FutureOr<Object?> handle(String op, Object? args, _Ctx ctx) {
    final a = (args as Map?) ?? const {};
    switch (op) {
      case 'plan':
        return lib.textJson(lib.ttsPlanFn, a['text'] as String);
      case 'load':
        if (_h == nullptr) {
          final path = a['path'] as String;
          final threads = a['threads'] as int;
          _h = lib.create((err) => using((arena) =>
              lib.ttsNew(path.toNativeUtf8(allocator: arena), threads, err)));
        }
        return lib.takeJson(lib.ttsInfo(_h));
      case 'unload':
        dispose();
        return null;
      case 'loaded':
        return _h != nullptr;
      case 'synth':
        if (_h == nullptr) throw const QazEngineError('voice not loaded');
        final piece = a['piece'] as String;
        final speaker = a['speaker'] as int;
        final speed = (a['speed'] as num).toDouble();
        return using((arena) {
          final n = arena<Size>()..value = 0;
          final err = arena<Pointer<Utf8>>()..value = nullptr;
          final p = lib.ttsSynth(
              _h, piece.toNativeUtf8(allocator: arena), speaker, speed, n, err);
          if (p == nullptr) {
            if (err.value != nullptr) {
              // Throws the engine's error; frees the string either way.
              lib.takeJson(err.value);
              throw const QazEngineError('synthesis failed', kind: 'inference');
            }
            return null;
          }
          final pcm = lib.takePcm(p, n.value);
          return TransferableTypedData.fromList([pcm]);
        });
    }
    throw QazEngineError('unknown op $op', kind: 'argument');
  }

  @override
  void dispose() {
    if (_h != nullptr) {
      lib.ttsFree(_h);
      _h = nullptr;
    }
  }
}

class _SttHandler extends _Handler {
  Pointer<Void> _h = nullptr;

  Pointer<Void> get _engine {
    if (_h == nullptr) throw const QazEngineError('recognizer not loaded');
    return _h;
  }

  @override
  Future<Object?> handle(String op, Object? args, _Ctx ctx) async {
    final a = (args as Map?) ?? const {};
    switch (op) {
      case 'load':
        if (_h == nullptr) {
          final cfg = a['config'] as String;
          _h = lib.create((err) => using(
              (arena) => lib.sttNew(cfg.toNativeUtf8(allocator: arena), err)));
        }
        return lib.takeJson(lib.sttInfo(_h));
      case 'unload':
        dispose();
        return null;
      case 'loaded':
        return _h != nullptr;
      case 'feed16':
        final data = (a['pcm'] as TransferableTypedData).materialize();
        return _feed16(
            data.asInt16List(), a['rate'] as int, a['channels'] as int);
      case 'feed32':
        final data = (a['pcm'] as TransferableTypedData).materialize();
        final f = data.asFloat32List();
        return using((arena) {
          final p = arena<Float>(f.isEmpty ? 1 : f.length);
          p.asTypedList(f.length).setAll(0, f);
          return lib.takeJson(lib.sttFeedF32(
              _engine, p, f.length, a['rate'] as int, a['channels'] as int));
        });
      case 'partial':
        return lib.takeJson(lib.sttPartial(_engine));
      case 'finish':
        return lib.takeJson(lib.sttFinish(_engine));
      case 'reset':
        lib.sttReset(_engine);
        return null;
      case 'hotwords':
        return using((arena) => lib.takeJson(lib.sttSetHotwords(
            _engine, (a['json'] as String).toNativeUtf8(allocator: arena))));
      case 'file':
        return _file(a, ctx);
    }
    throw QazEngineError('unknown op $op', kind: 'argument');
  }

  Object? _feed16(Int16List pcm, int rate, int channels) => using((arena) {
        final p = arena<Int16>(pcm.isEmpty ? 1 : pcm.length);
        p.asTypedList(pcm.length).setAll(0, pcm);
        return lib
            .takeJson(lib.sttFeedI16(_engine, p, pcm.length, rate, channels));
      });

  /// Transcribes raw 16-bit little-endian PCM from a file, in chunks, with
  /// progress, checking for cancellation between chunks.
  Future<Object?> _file(Map a, _Ctx ctx) async {
    final path = a['path'] as String;
    final rate = a['rate'] as int;
    final channels = a['channels'] as int;
    final offset = a['offset'] as int;
    final length = a['length'] as int?;
    final engine = _engine;
    final raf = File(path).openSync();
    final finals = <Object?>[];
    lib.sttReset(engine);
    try {
      final end = length != null ? offset + length : raf.lengthSync();
      final total = (end - offset).clamp(1, 1 << 62);
      var pos = offset;
      raf.setPositionSync(pos);
      // ~5 s of audio per chunk: progress updates often, cancellation is quick.
      final chunk = (rate * channels * 2 * 5) & ~1;
      var lastProgress = DateTime.now();
      while (pos < end) {
        ctx.checkCancelled();
        final want = (end - pos).clamp(0, chunk);
        final bytes = raf.readSync(want);
        if (bytes.isEmpty) break;
        pos += bytes.length;
        final even = bytes.length & ~1;
        final pcm = Int16List.view(
            Uint8List.fromList(bytes.sublist(0, even)).buffer, 0, even ~/ 2);
        final r = _feed16(pcm, rate, channels) as Map;
        finals.addAll(r['finals'] as List);
        final now = DateTime.now();
        if (now.difference(lastProgress).inMilliseconds >= 200) {
          lastProgress = now;
          ctx.progress(
              {'fraction': (pos - offset) / total, 'finals': r['finals']});
        } else if ((r['finals'] as List).isNotEmpty) {
          ctx.progress(
              {'fraction': (pos - offset) / total, 'finals': r['finals']});
        }
        // Let a cancel message in.
        await Future<void>.delayed(Duration.zero);
      }
      ctx.checkCancelled();
      final r = lib.takeJson(lib.sttFinish(engine)) as Map;
      finals.addAll(r['finals'] as List);
      return finals;
    } catch (_) {
      lib.sttReset(engine);
      rethrow;
    } finally {
      raf.closeSync();
    }
  }

  @override
  void dispose() {
    if (_h != nullptr) {
      lib.sttFree(_h);
      _h = nullptr;
    }
  }
}

class _PunctHandler extends _Handler {
  Pointer<Void> _h = nullptr;

  @override
  FutureOr<Object?> handle(String op, Object? args, _Ctx ctx) {
    final a = (args as Map?) ?? const {};
    switch (op) {
      case 'load':
        if (_h == nullptr) {
          final dir = a['path'] as String;
          final threads = a['threads'] as int;
          _h = lib.create((err) => using((arena) =>
              lib.punctNew(dir.toNativeUtf8(allocator: arena), threads, err)));
        }
        return null;
      case 'unload':
        dispose();
        return null;
      case 'loaded':
        return _h != nullptr;
      case 'apply':
        if (_h == nullptr) throw const QazEngineError('punctuation not loaded');
        final r = using((arena) => lib.takeJson(lib.punctApply(
            _h, (a['text'] as String).toNativeUtf8(allocator: arena)))) as Map;
        return r['text'];
    }
    throw QazEngineError('unknown op $op', kind: 'argument');
  }

  @override
  void dispose() {
    if (_h != nullptr) {
      lib.punctFree(_h);
      _h = nullptr;
    }
  }
}
