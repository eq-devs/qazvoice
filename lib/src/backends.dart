import 'dart:async';
import 'dart:convert';
import 'dart:isolate';
import 'dart:math' as math;
import 'dart:typed_data';

import 'errors.dart';
import 'native/worker.dart';

/// One synthesis step from the TTS planner.
class PlanItem {
  const PlanItem(this.piece, this.sentence, this.start, this.end, this.pause);
  final String piece, sentence;
  final int start, end;
  final bool pause;
}

/// Voice facts known after loading.
class TtsInfo {
  const TtsInfo(this.sampleRate, this.speakers);
  final int sampleRate;
  final List<String> speakers;
}

/// One recognized piece as returned by the engine.
class RawResult {
  const RawResult(this.text, this.startMs, this.endMs);
  final String text;
  final int startMs, endMs;
}

class FeedResult {
  const FeedResult(this.finals, this.speaking);
  final List<RawResult> finals;
  final bool speaking;
}

/// Text-to-speech engine as the Dart side sees it (native or mock).
abstract class TtsBackend {
  bool get isLoaded;
  Future<TtsInfo> load(String voicePath, int threads);
  Future<void> unload();
  Future<List<PlanItem>> plan(String text);
  Future<Int16List> synth(String piece, int speaker, double speed);
  Future<void> dispose();
}

/// Speech recognition engine (native or mock).
abstract class SttBackend {
  bool get isLoaded;
  Future<Map<String, Object?>> load(Map<String, Object?> config);
  Future<void> unload();
  Future<FeedResult> feed16(Int16List pcm, int rate, int channels);
  Future<FeedResult> feed32(Float32List pcm, int rate, int channels);
  Future<RawResult?> partial();
  Future<List<RawResult>> finish();
  Future<void> reset();
  Future<void> setHotwords(List<String> words);

  /// Raw s16le PCM in [path] from byte [offset] ([length] bytes or to the end).
  Future<List<RawResult>> transcribePcmFile(String path,
      {required int rate,
      required int channels,
      int offset = 0,
      int? length,
      void Function(double fraction, List<RawResult> finals)? onProgress,
      QazCancelToken? cancelToken});
  Future<void> dispose();
}

/// Punctuation model (native or mock).
abstract class PunctBackend {
  bool get isLoaded;
  Future<void> load(String dir, int threads);
  Future<void> unload();
  Future<String> apply(String text);
  Future<void> dispose();
}

List<RawResult> _results(Object? list) => [
      for (final m in (list as List? ?? const []).cast<Map>())
        RawResult('${m['text']}', (m['start_ms'] as num).toInt(),
            (m['end_ms'] as num).toInt())
    ];

FeedResult _feed(Object? r) {
  final m = r as Map;
  return FeedResult(_results(m['finals']), m['speaking'] == true);
}

/// Lazily spawned worker isolate, shared by all calls of one backend.
abstract class _WorkerBackend {
  _WorkerBackend(this._kind);
  final String _kind;
  NativeWorker? _w;
  Future<NativeWorker>? _spawning;
  bool _loaded = false;
  bool _disposed = false;
  bool get isLoaded => _loaded;

  Future<NativeWorker> get worker async {
    if (_disposed) throw const QazEngineError('engine is closed');
    final w = _w;
    if (w != null) return w;
    final spawned = await (_spawning ??= NativeWorker.spawn(_kind));
    if (_disposed) {
      // Closed while the isolate was starting.
      await spawned.dispose();
      throw const QazEngineError('engine is closed');
    }
    return _w = spawned;
  }

  Future<Object?> call(String op,
          [Object? args,
          void Function(Object?)? onProgress,
          QazCancelToken? token]) async =>
      (await worker).call(op, args, onProgress, token);

  Future<void> unload() async {
    if (_w == null) return;
    // Mark unloaded first: a request arriving meanwhile reloads (load is a
    // no-op in the worker while the handle still exists).
    _loaded = false;
    await call('unload');
  }

  Future<void> dispose() async {
    _disposed = true;
    var w = _w;
    final spawning = _spawning;
    _w = null;
    _spawning = null;
    _loaded = false;
    if (w == null && spawning != null) {
      try {
        w = await spawning;
      } catch (_) {}
    }
    await w?.dispose();
  }
}

class NativeTtsBackend extends _WorkerBackend implements TtsBackend {
  NativeTtsBackend() : super('tts');

  @override
  Future<TtsInfo> load(String voicePath, int threads) async {
    final r =
        await call('load', {'path': voicePath, 'threads': threads}) as Map;
    _loaded = true;
    return TtsInfo((r['sample_rate'] as num).toInt(),
        [for (final s in r['speakers'] as List) '$s']);
  }

  @override
  Future<List<PlanItem>> plan(String text) async {
    final r = await call('plan', {'text': text}) as List;
    return [
      for (final m in r.cast<Map>())
        PlanItem(
            '${m['piece']}',
            '${m['sentence']}',
            (m['start'] as num).toInt(),
            (m['end'] as num).toInt(),
            m['pause'] == true)
    ];
  }

  @override
  Future<Int16List> synth(String piece, int speaker, double speed) async {
    final r = await call(
        'synth', {'piece': piece, 'speaker': speaker, 'speed': speed});
    if (r == null) return Int16List(0);
    return (r as TransferableTypedData).materialize().asInt16List();
  }
}

class NativeSttBackend extends _WorkerBackend implements SttBackend {
  NativeSttBackend() : super('stt');

  @override
  Future<Map<String, Object?>> load(Map<String, Object?> config) async {
    final r = await call('load', {'config': jsonEncode(config)}) as Map;
    _loaded = true;
    return r.cast<String, Object?>();
  }

  @override
  Future<FeedResult> feed16(Int16List pcm, int rate, int channels) async =>
      _feed(await call('feed16', {
        'pcm': TransferableTypedData.fromList([pcm]),
        'rate': rate,
        'channels': channels
      }));

  @override
  Future<FeedResult> feed32(Float32List pcm, int rate, int channels) async =>
      _feed(await call('feed32', {
        'pcm': TransferableTypedData.fromList([pcm]),
        'rate': rate,
        'channels': channels
      }));

  @override
  Future<RawResult?> partial() async {
    final r = await call('partial') as Map;
    final p = r['partial'];
    return p == null ? null : _results([p]).first;
  }

  @override
  Future<List<RawResult>> finish() async =>
      _results((await call('finish') as Map)['finals']);

  @override
  Future<void> reset() => call('reset');

  @override
  Future<void> setHotwords(List<String> words) =>
      call('hotwords', {'json': jsonEncode(words)});

  @override
  Future<List<RawResult>> transcribePcmFile(String path,
      {required int rate,
      required int channels,
      int offset = 0,
      int? length,
      void Function(double fraction, List<RawResult> finals)? onProgress,
      QazCancelToken? cancelToken}) async {
    final r = await call(
        'file',
        {
          'path': path,
          'rate': rate,
          'channels': channels,
          'offset': offset,
          'length': length
        },
        onProgress == null
            ? null
            : (p) {
                final m = p as Map;
                onProgress(
                    (m['fraction'] as num).toDouble(), _results(m['finals']));
              },
        cancelToken);
    return _results(r);
  }
}

class NativePunctBackend extends _WorkerBackend implements PunctBackend {
  NativePunctBackend() : super('punct');

  @override
  Future<void> load(String dir, int threads) async {
    await call('load', {'path': dir, 'threads': threads});
    _loaded = true;
  }

  @override
  Future<String> apply(String text) async =>
      '${await call('apply', {'text': text})}';
}

// ───────────────────────────── mocks ─────────────────────────────

/// Fake voice: a tone per piece, length proportional to the text.
class MockTtsBackend implements TtsBackend {
  MockTtsBackend({this.sampleRate = 22050});
  final int sampleRate;
  bool _loaded = false;
  @override
  bool get isLoaded => _loaded;

  @override
  Future<TtsInfo> load(String voicePath, int threads) async {
    await Future<void>.delayed(const Duration(milliseconds: 20));
    _loaded = true;
    return TtsInfo(sampleRate, const ['F1', 'F2', 'F3', 'M1', 'M2', 'M3']);
  }

  @override
  Future<void> unload() async => _loaded = false;

  @override
  Future<List<PlanItem>> plan(String text) async {
    final out = <PlanItem>[];
    final re = RegExp(r'[^.!?…]+[.!?…]*');
    for (final m in re.allMatches(text)) {
      final s = m.group(0)!;
      final lead = s.length - s.trimLeft().length;
      final t = s.trim();
      if (t.isEmpty) continue;
      final start = m.start + lead;
      out.add(PlanItem(t.toLowerCase(), t, start, start + t.length, true));
    }
    return out;
  }

  @override
  Future<Int16List> synth(String piece, int speaker, double speed) async {
    if (!_loaded) throw const QazEngineError('voice not loaded');
    final n =
        (sampleRate * 0.05 * piece.length / speed.clamp(0.5, 2.0)).round();
    final f = 180.0 + 20 * speaker;
    return Int16List.fromList(List.generate(
        n, (i) => (math.sin(2 * math.pi * f * i / sampleRate) * 8000).round()));
  }

  @override
  Future<void> dispose() async => _loaded = false;
}

/// Fake recognizer: "hears" [text] for every stretch of loud audio.
class MockSttBackend implements SttBackend {
  MockSttBackend({this.text = 'сәлем әлем'});
  final String text;
  bool _loaded = false;
  bool _speaking = false;
  int _samples = 0, _speechStart = 0, _silent = 0;
  int _minSilence = 9600;
  @override
  bool get isLoaded => _loaded;

  @override
  Future<Map<String, Object?>> load(Map<String, Object?> config) async {
    _loaded = true;
    final ms = (config['min_silence_ms'] as int?) ?? 0;
    _minSilence = (ms > 0 ? ms : 600) * 16;
    return {
      'vad': 'mock',
      'punctuation': config['punct_model_path'] != null,
      'backend': 'mock'
    };
  }

  @override
  Future<void> unload() async => _loaded = false;

  @override
  Future<FeedResult> feed16(Int16List pcm, int rate, int channels) async {
    final f = Float32List(pcm.length);
    for (var i = 0; i < pcm.length; i++) {
      f[i] = pcm[i] / 32768.0;
    }
    return feed32(f, rate, channels);
  }

  @override
  Future<FeedResult> feed32(Float32List pcm, int rate, int channels) async {
    if (!_loaded) throw const QazEngineError('recognizer not loaded');
    final finals = <RawResult>[];
    final step = channels * (rate / 16000);
    for (var i = 0.0; i < pcm.length; i += step) {
      final loud = pcm[i.floor()].abs() > 0.02;
      _samples++;
      if (loud) {
        if (!_speaking) {
          _speaking = true;
          _speechStart = _samples;
        }
        _silent = 0;
      } else if (_speaking && ++_silent >= _minSilence) {
        _speaking = false;
        finals.add(_result());
      }
    }
    return FeedResult(finals, _speaking);
  }

  RawResult _result() => RawResult(
      _capitalize(text), _speechStart ~/ 16, (_samples - _silent) ~/ 16);

  static String _capitalize(String s) =>
      s.isEmpty ? s : s[0].toUpperCase() + s.substring(1);

  @override
  Future<RawResult?> partial() async =>
      _speaking && _samples - _speechStart > 4800
          ? RawResult(_capitalize(text.split(' ').first), _speechStart ~/ 16,
              _samples ~/ 16)
          : null;

  @override
  Future<List<RawResult>> finish() async {
    final out = _speaking ? [_result()] : <RawResult>[];
    await reset();
    return out;
  }

  @override
  Future<void> reset() async {
    _speaking = false;
    _samples = _speechStart = _silent = 0;
  }

  @override
  Future<void> setHotwords(List<String> words) async {}

  @override
  Future<List<RawResult>> transcribePcmFile(String path,
      {required int rate,
      required int channels,
      int offset = 0,
      int? length,
      void Function(double fraction, List<RawResult> finals)? onProgress,
      QazCancelToken? cancelToken}) async {
    for (var i = 1; i <= 4; i++) {
      await Future<void>.delayed(const Duration(milliseconds: 10));
      if (cancelToken?.isCancelled ?? false) throw const QazCancelled();
      onProgress?.call(i / 4, const []);
    }
    return [RawResult(_capitalize(text), 0, 1000)];
  }

  @override
  Future<void> dispose() async => _loaded = false;
}

/// Fake punctuation: capitalizes and adds a full stop.
class MockPunctBackend implements PunctBackend {
  bool _loaded = false;
  @override
  bool get isLoaded => _loaded;
  @override
  Future<void> load(String dir, int threads) async => _loaded = true;
  @override
  Future<void> unload() async => _loaded = false;
  @override
  Future<String> apply(String text) async {
    final t = text.trim();
    if (t.isEmpty) return t;
    final c = t[0].toUpperCase() + t.substring(1);
    return RegExp(r'[.!?]$').hasMatch(c) ? c : '$c.';
  }

  @override
  Future<void> dispose() async => _loaded = false;
}
