import 'dart:async';
import 'dart:io';
import 'dart:math' as math;
import 'dart:typed_data';

import 'package:flutter/services.dart';

import 'errors.dart';

/// Everything the Dart side needs from the host platform: model storage and
/// the background downloader, audio playback, microphone, decoding, device facts.
abstract class QazPlatform {
  /// Returns `{modelsDir, states: {id: state}}`. Re-attaches to downloads that
  /// were running when the app was last closed.
  Future<Map<String, Object?>> init(Map<String, Object?> config);

  /// Download updates (`{type: download, id, status, …}`), memory pressure
  /// (`{type: trimMemory, level}`), playback progress (`{type: player, …}`)
  /// and decoder progress (`{type: decode, …}`).
  Stream<Map<String, Object?>> get events;

  Future<Map<String, Object?>> device();
  Future<Map<String, Object?>> modelState(String id);
  Future<void> download(Map<String, Object?> spec);
  Future<void> pause(String id);
  Future<void> resume(String id);
  Future<void> cancel(String id);
  Future<void> delete(String id);
  Future<int> storageUsed();
  Future<bool> requestNotificationPermission();
  Future<bool> requestMicrophonePermission();

  Future<int> playerCreate(int sampleRate);

  /// Queues PCM; completes once the player has room for more (back-pressure).
  Future<void> playerWrite(int id, Uint8List pcm16);

  /// Completes when everything queued has been played.
  Future<void> playerFinish(int id);
  Future<void> playerStop(int id);

  /// 16-bit mono microphone audio in ~100 ms chunks while listened to.
  Stream<Uint8List> microphone(int sampleRate);

  /// Decodes any audio/video file to raw s16le PCM in [output].
  /// Returns `{rate, channels, durationMs, bytes}`.
  Future<Map<String, Object?>> decode(String input, String output, int token);
  Future<void> cancelDecode(int token);

  /// Encodes raw s16le mono PCM to AAC in an .m4a container.
  Future<void> encodeM4a(String pcmPath, int sampleRate, String output);

  Future<String> tempDir();
}

Never _rethrow(PlatformException e) {
  throw errorFromWire({
    'kind': e.code,
    'message': e.message ?? e.code,
    ...?(e.details is Map ? (e.details as Map).cast<String, Object?>() : null),
  });
}

Future<T> _guard<T>(Future<T> f) async {
  try {
    return await f;
  } on PlatformException catch (e) {
    _rethrow(e);
  } on MissingPluginException {
    throw const QazUnsupportedDevice(
        'qazvoice is not available on this platform');
  }
}

/// The real implementation (Android: `QazvoicePlugin.kt`).
class MethodChannelQazPlatform extends QazPlatform {
  static const _m = MethodChannel('kz.qazvoice/main');
  static const _events = EventChannel('kz.qazvoice/events');
  static const _mic = EventChannel('kz.qazvoice/mic');

  Stream<Map<String, Object?>>? _ev;

  Future<T?> _call<T>(String method, [Object? args]) =>
      _guard(_m.invokeMethod<T>(method, args));

  Future<Map<String, Object?>> _map(String method, [Object? args]) async =>
      ((await _call<Map>(method, args)) ?? const {}).cast<String, Object?>();

  @override
  Future<Map<String, Object?>> init(Map<String, Object?> config) =>
      _map('init', config);

  @override
  Stream<Map<String, Object?>> get events => _ev ??= _events
      .receiveBroadcastStream()
      .map((e) => (e as Map).cast<String, Object?>())
      .asBroadcastStream();

  @override
  Future<Map<String, Object?>> device() => _map('device');

  @override
  Future<Map<String, Object?>> modelState(String id) =>
      _map('modelState', {'id': id});

  @override
  Future<void> download(Map<String, Object?> spec) => _call('download', spec);

  @override
  Future<void> pause(String id) => _call('pause', {'id': id});

  @override
  Future<void> resume(String id) => _call('resume', {'id': id});

  @override
  Future<void> cancel(String id) => _call('cancel', {'id': id});

  @override
  Future<void> delete(String id) => _call('delete', {'id': id});

  @override
  Future<int> storageUsed() async => (await _call<int>('storageUsed')) ?? 0;

  @override
  Future<bool> requestNotificationPermission() async =>
      (await _call<bool>('requestNotificationPermission')) ?? false;

  @override
  Future<bool> requestMicrophonePermission() async =>
      (await _call<bool>('requestMicrophonePermission')) ?? false;

  @override
  Future<int> playerCreate(int sampleRate) async =>
      (await _call<int>('playerCreate', {'rate': sampleRate}))!;

  @override
  Future<void> playerWrite(int id, Uint8List pcm16) =>
      _call('playerWrite', {'id': id, 'pcm': pcm16});

  @override
  Future<void> playerFinish(int id) => _call('playerFinish', {'id': id});

  @override
  Future<void> playerStop(int id) => _call('playerStop', {'id': id});

  @override
  Stream<Uint8List> microphone(int sampleRate) =>
      _mic.receiveBroadcastStream({'rate': sampleRate}).handleError((Object e) {
        if (e is PlatformException) _rethrow(e);
        throw e;
      }).map((e) => e as Uint8List);

  @override
  Future<Map<String, Object?>> decode(String input, String output, int token) =>
      _map('decode', {'input': input, 'output': output, 'token': token});

  @override
  Future<void> cancelDecode(int token) =>
      _call('cancelDecode', {'token': token});

  @override
  Future<void> encodeM4a(String pcmPath, int sampleRate, String output) =>
      _call('encodeM4a',
          {'input': pcmPath, 'rate': sampleRate, 'output': output});

  @override
  Future<String> tempDir() async => (await _call<String>('tempDir'))!;
}

/// In-memory platform for tests and `QazVoice.useMock()`: downloads finish in
/// a few ticks, playback runs [playbackSpeed]× real time, the microphone
/// plays [micPcm] (or a tone with pauses).
class MockQazPlatform extends QazPlatform {
  MockQazPlatform({
    this.downloadTicks = 5,
    this.tick = const Duration(milliseconds: 20),
    this.playbackSpeed = 50,
    this.micPcm,
    this.freeBytes = 8 << 30,
    this.modelsDir,
    Map<String, int> installed = const {},
  }) {
    installed.forEach((id, version) => _states[id] = {
          'status': 'installed',
          'version': version,
          'installedVersion': version,
          'installedBytes': 0,
        });
  }

  /// Where models "are"; a temp directory by default. Point it at real model
  /// files to run the native engines on a desktop host.
  final String? modelsDir;

  final int downloadTicks;
  final Duration tick;
  final double playbackSpeed;
  final Int16List? micPcm;
  int freeBytes;

  /// Makes the next download of a model fail with this error.
  final Map<String, QazException> failNext = {};

  final _ev = StreamController<Map<String, Object?>>.broadcast();
  final Map<String, Map<String, Object?>> _states = {};
  final Map<String, Timer> _timers = {};
  final Map<String, int> _sizes = {};
  int _nextPlayer = 0;
  Directory? _tmp;

  @override
  Future<Map<String, Object?>> init(Map<String, Object?> config) async {
    _tmp ??= Directory.systemTemp.createTempSync('qazvoice_mock');
    return {
      'modelsDir': modelsDir ?? '${_tmp!.path}/models',
      'states': Map.of(_states)
    };
  }

  @override
  Stream<Map<String, Object?>> get events => _ev.stream;

  /// Simulates Android's onTrimMemory (memory pressure).
  void emitTrimMemory([int level = 15]) =>
      _ev.add({'type': 'trimMemory', 'level': level});

  void _emit(String id, Map<String, Object?> s) {
    _states[id] = s;
    _ev.add({'type': 'download', 'id': id, ...s});
  }

  @override
  Future<Map<String, Object?>> device() async => {
        'abis': ['arm64-v8a'],
        'totalRam': 4 << 30,
        'availRam': 2 << 30,
        'lowRam': false,
        'freeBytes': freeBytes,
        'sdk': 34,
        'model': 'mock',
      };

  @override
  Future<Map<String, Object?>> modelState(String id) async =>
      _states[id] ?? {'status': 'notInstalled', 'received': 0};

  @override
  Future<void> download(Map<String, Object?> spec) async {
    final id = spec['id'] as String;
    if (_timers.containsKey(id)) return;
    _specs[id] = spec;
    final files = (spec['files'] as List).cast<Map>();
    final total = files.fold<int>(0, (a, f) => a + (f['size'] as int));
    _sizes[id] = total;
    if (total > freeBytes) {
      _emit(id, {
        'status': 'failed',
        'received': 0,
        'total': total,
        'error': errorToWire(
            QazInsufficientStorage(total, availableBytes: freeBytes)),
      });
      return;
    }
    final fail = failNext.remove(id);
    var received = (_states[id]?['received'] as int?) ?? 0;
    var n = (received * downloadTicks / math.max(total, 1)).floor();
    _timers[id] = Timer.periodic(tick, (t) {
      n++;
      received = math.min(total, (total * n / downloadTicks).round());
      if (fail != null && n >= downloadTicks ~/ 2) {
        t.cancel();
        _timers.remove(id);
        _emit(id, {
          'status': 'failed',
          'received': received,
          'total': total,
          'error': errorToWire(fail)
        });
        return;
      }
      if (received >= total) {
        t.cancel();
        _timers.remove(id);
        _emit(id, {'status': 'verifying', 'received': total, 'total': total});
        _emit(id, {
          'status': 'installed',
          'version': spec['version'],
          'installedBytes': total,
        });
        return;
      }
      _emit(id, {
        'status': 'downloading',
        'received': received,
        'total': total,
        'speed': total / downloadTicks / (tick.inMicroseconds / 1e6),
      });
    });
  }

  @override
  Future<void> pause(String id) async {
    _timers.remove(id)?.cancel();
    final s = _states[id];
    if (s != null && s['status'] == 'downloading') {
      _emit(id, {...s, 'status': 'paused'});
    }
  }

  final Map<String, Map<String, Object?>> _specs = {};

  @override
  Future<void> resume(String id) async {
    final s = _states[id];
    final spec = _specs[id];
    if (s == null || spec == null || s['status'] != 'paused') return;
    await download(spec);
  }

  @override
  Future<void> cancel(String id) async {
    _timers.remove(id)?.cancel();
    _emit(id, {'status': 'notInstalled', 'received': 0});
  }

  @override
  Future<void> delete(String id) async {
    _timers.remove(id)?.cancel();
    _emit(id, {'status': 'notInstalled', 'received': 0});
  }

  @override
  Future<int> storageUsed() async => _states.entries
      .where((e) => e.value['status'] == 'installed')
      .fold<int>(0, (a, e) => a + (_sizes[e.key] ?? 0));

  @override
  Future<bool> requestNotificationPermission() async => true;

  bool micAllowed = true;

  @override
  Future<bool> requestMicrophonePermission() async => micAllowed;

  final Map<int, _MockPlayer> _players = {};

  @override
  Future<int> playerCreate(int sampleRate) async {
    final id = _nextPlayer++;
    _players[id] = _MockPlayer(sampleRate);
    return id;
  }

  @override
  Future<void> playerWrite(int id, Uint8List pcm16) async {
    final p = _players[id];
    if (p == null) return;
    p.frames += pcm16.length ~/ 2;
    _ev.add({'type': 'player', 'id': id, 'frames': p.played, 'level': 0.5});
  }

  @override
  Future<void> playerFinish(int id) async {
    final p = _players[id];
    if (p == null) return;
    final step = (p.rate * tick.inMicroseconds / 1e6 * playbackSpeed).round();
    while (!p.stopped && p.played < p.frames) {
      await Future<void>.delayed(tick);
      p.played = math.min(p.frames, p.played + step);
      _ev.add({'type': 'player', 'id': id, 'frames': p.played, 'level': 0.4});
    }
    _ev.add({'type': 'player', 'id': id, 'frames': p.played, 'level': 0.0});
    _players.remove(id);
  }

  @override
  Future<void> playerStop(int id) async {
    _players.remove(id)?.stopped = true;
  }

  @override
  Stream<Uint8List> microphone(int sampleRate) {
    late StreamController<Uint8List> c;
    Timer? t;
    c = StreamController<Uint8List>(
        onListen: () async {
          if (!micAllowed) {
            c.addError(const QazPermissionDenied('microphone'));
            await c.close();
            return;
          }
          final src = micPcm ?? _toneWithPauses(sampleRate);
          var pos = 0;
          final chunk = sampleRate ~/ 10;
          t = Timer.periodic(tick, (_) {
            if (pos >= src.length) {
              // Keep sending silence, like a real microphone.
              c.add(Uint8List(chunk * 2));
              return;
            }
            final end = math.min(src.length, pos + chunk);
            c.add(Uint8List.fromList(Int16List.fromList(src.sublist(pos, end))
                .buffer
                .asUint8List()));
            pos = end;
          });
        },
        onCancel: () => t?.cancel());
    return c.stream;
  }

  static Int16List _toneWithPauses(int rate) {
    final out = Int16List(rate * 3);
    for (var i = rate ~/ 2; i < rate * 3 ~/ 2; i++) {
      out[i] = (math.sin(2 * math.pi * 200 * i / rate) * 8000).round();
    }
    return out;
  }

  @override
  Future<Map<String, Object?>> decode(
      String input, String output, int token) async {
    throw const QazEngineError('mock platform decodes only .wav',
        kind: 'audio');
  }

  @override
  Future<void> cancelDecode(int token) async {}

  @override
  Future<void> encodeM4a(String pcmPath, int sampleRate, String output) async {
    await File(output).writeAsBytes(await File(pcmPath).readAsBytes());
  }

  @override
  Future<String> tempDir() async {
    _tmp ??= Directory.systemTemp.createTempSync('qazvoice_mock');
    return _tmp!.path;
  }

  /// For tests: the last download spec sent for [id].
  Map<String, Object?>? specOf(String id) => _specs[id];
}

class _MockPlayer {
  _MockPlayer(this.rate);
  final int rate;
  int frames = 0, played = 0;
  bool stopped = false;
}
