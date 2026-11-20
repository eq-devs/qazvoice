import 'dart:async';
import 'dart:io';
import 'dart:typed_data';

import 'package:flutter/foundation.dart';

import 'audio.dart';
import 'backends.dart';
import 'catalog.dart';
import 'engine.dart';
import 'errors.dart';
import 'voice.dart';

/// Recognized text. Interim results ([isFinal] false) are replaced by later
/// ones for the same utterance; finals are fixed.
@immutable
class SttResult {
  const SttResult(
      {required this.text,
      required this.isFinal,
      required this.startMs,
      required this.endMs});

  final String text;
  final bool isFinal;

  /// Position in the audio stream (ms from the first sample fed).
  final int startMs, endMs;

  Duration get start => Duration(milliseconds: startMs);
  Duration get end => Duration(milliseconds: endMs);

  @override
  bool operator ==(Object other) =>
      other is SttResult &&
      other.text == text &&
      other.isFinal == isFinal &&
      other.startMs == startMs &&
      other.endMs == endMs;

  @override
  int get hashCode => Object.hash(text, isFinal, startMs, endMs);

  @override
  String toString() =>
      'SttResult(${isFinal ? 'final' : 'interim'} $startMs–$endMs "$text")';
}

/// Voice activity changes in a session.
enum SttEvent {
  /// Speech began (an utterance opened).
  speechStart,

  /// The utterance ended (a pause long enough to finalize it).
  speechEnd,
}

/// On-device Kazakh speech recognition (model `stt-kk`, optional `punct-kk`).
///
/// ```dart
/// final stt = await QazStt.open(punctuation: true);
/// final session = await stt.listen(autoStopAfter: const Duration(seconds: 2));
/// session.results.listen((r) => print(r.text));
/// ```
class QazStt extends ModelEngine {
  QazStt._(this._backend, this._config, this._usesPunct, this._modelId,
      Duration? idleTimeout)
      : super(idleTimeout);

  final String _modelId;

  /// The recognition model this engine uses.
  String get model => _modelId;

  /// The default recognizer.
  static const modelId = 'stt-kk';

  /// Optional, more accurate recognizer (GigaAM Multilingual, MIT): about
  /// 24% fewer word errors on Kazakh read speech, ~1.6× slower, 253 MB download.
  static const highAccuracyModelId = 'stt-kk-giga';
  static const punctModelId = 'punct-kk';

  final SttBackend _backend;
  final Map<String, Object?> _config;
  final bool _usesPunct;
  SttSession? _session;
  _Listen? _listen;
  final ValueNotifier<bool> _listening = ValueNotifier(false);
  static int _decodeToken = 0;

  /// Opens the recognizer. Throws [QazModelNotInstalled] if `stt-kk` (or
  /// `punct-kk` with [punctuation]) is not installed.
  ///
  /// * [punctuation] — restore punctuation and capitals with `punct-kk`.
  /// * [numbersToDigits] — "жиырма бес" → "25".
  /// * [latin] — output in the 2021 Latin alphabet.
  /// * [capitalize] — capitalize sentence starts (when [punctuation] is off).
  /// * [hotwords] — names/terms to snap near-misses onto.
  /// * [minSilence] — pause that ends an utterance (default 600 ms).
  /// * [maxSegment] — longest utterance before a forced cut (default 25 s).
  static Future<QazStt> open({
    String model = modelId,
    bool punctuation = false,
    bool numbersToDigits = false,
    bool latin = false,
    bool capitalize = true,
    List<String> hotwords = const [],
    Duration? minSilence,
    Duration? maxSegment,
    int threads = 0,
    Duration? idleTimeout = const Duration(minutes: 3),
    bool preload = false,
  }) async {
    final internals = QazVoice.internals;
    await internals.ensureInit();
    final m = internals.model(model)..ensureInstalled();
    if (m.kind != QazModelKind.stt) {
      throw ArgumentError.value(
          model, 'model', 'not a speech recognition model');
    }
    String? punctDir;
    if (punctuation) {
      final p = internals.model(punctModelId)..ensureInstalled();
      punctDir = p.directory;
    }
    final t = threads > 0 ? threads : Platform.numberOfProcessors.clamp(2, 4);
    final config = <String, Object?>{
      'model_path': m.directory,
      if (punctDir != null) 'punct_model_path': punctDir,
      'latin': latin,
      'numbers_to_digits': numbersToDigits,
      'capitalize': capitalize,
      'hotwords': hotwords,
      'threads': t,
      'min_silence_ms': minSilence?.inMilliseconds ?? 0,
      'max_segment_ms': maxSegment?.inMilliseconds ?? 0,
    };
    final stt =
        QazStt._(internals.newStt(), config, punctuation, model, idleTimeout);
    internals.register(stt);
    if (preload) await stt.preload();
    return stt;
  }

  @override
  Iterable<String> get modelIds => [_modelId, if (_usesPunct) punctModelId];

  @override
  bool get isLoaded => _backend.isLoaded;

  @override
  bool get pinned => _session != null;

  @override
  Future<void> doLoad() async {
    await _backend.load(_config);
  }

  @override
  Future<void> doUnload() => _backend.unload();

  @override
  Future<void> doClose() async {
    await stopListening();
    await _session?.cancel();
    await _backend.dispose();
  }

  /// Replaces the hotword list of the loaded (or next loaded) model.
  Future<void> setHotwords(List<String> hotwords) {
    _config['hotwords'] = List<String>.of(hotwords);
    if (!isLoaded) return Future.value();
    return use(() => _backend.setHotwords(hotwords));
  }

  /// A recognition stream you feed yourself (from your own recorder, a
  /// network stream…). Starting a new session cancels the previous one.
  SttSession session() {
    checkOpen();
    final old = _session;
    final s = _session = SttSession._(this);
    if (old != null) {
      // The old session's reset must reach the engine before our first audio.
      s._feeding = old.cancel().catchError((Object _) {});
    }
    return s;
  }

  /// Whether [listen] is recording.
  bool get isListening => _listening.value;

  /// True while [listen] records.
  ValueListenable<bool> get listening => _listening;

  /// Records from the microphone (16 kHz, voice-recognition source) into a
  /// new session. Asks for the microphone permission (throws
  /// [QazPermissionDenied]).
  ///
  /// [autoStopAfter] stops after that much silence following speech;
  /// [maxDuration] stops after that long in any case.
  Future<SttSession> listen(
      {Duration? autoStopAfter, Duration? maxDuration}) async {
    checkOpen();
    await stopListening();
    final p = QazVoice.internals.platform;
    if (!await p.requestMicrophonePermission()) {
      throw const QazPermissionDenied('microphone');
    }
    await preload();
    final s = session();
    final l = _listen = _Listen(s);
    _listening.value = true;
    final started = DateTime.now();
    DateTime? silentSince;
    l.sub = p.microphone(16000).listen((bytes) {
      if (l.stopping) return;
      final pcm = bytesToPcm16(bytes);
      l.pending = l.pending.then((_) => s.addPcm16(pcm)).catchError((Object e) {
        s._fail(e);
      });
      final now = DateTime.now();
      if (maxDuration != null && now.difference(started) >= maxDuration) {
        unawaited(stopListening());
        return;
      }
      if (autoStopAfter != null) {
        if (s._speaking) {
          silentSince = null;
        } else if (s._heardSpeech) {
          silentSince ??= now;
          if (now.difference(silentSince!) >= autoStopAfter) {
            unawaited(stopListening());
          }
        }
      }
    }, onError: (Object e) {
      s._fail(e is QazException ? e : QazEngineError('$e', kind: 'audio'));
      unawaited(stopListening());
    });
    return s;
  }

  /// Stops [listen]: the microphone closes, the last utterance is finalized
  /// and the session's streams close.
  Future<void> stopListening() async {
    final l = _listen;
    if (l == null) return;
    _listen = null;
    l.stopping = true;
    await l.sub?.cancel();
    await l.pending;
    if (!l.session._closed) {
      try {
        await l.session.finish();
      } catch (_) {}
    }
    _listening.value = false;
  }

  /// Transcribes an audio or video file (wav, mp3, m4a/aac, ogg/opus, flac,
  /// mp4…; anything the platform can decode). [onProgress] gets 0..1;
  /// [onResult] gets each final as it is recognized. Cancel with [cancelToken].
  Future<List<SttResult>> transcribeFile(String path,
      {void Function(double progress)? onProgress,
      void Function(SttResult result)? onResult,
      QazCancelToken? cancelToken}) async {
    checkOpen();
    if (_session != null) {
      throw StateError('a recognition session is active; finish it first');
    }
    final file = File(path);
    if (!await file.exists()) {
      throw QazEngineError('file not found: $path', kind: 'audio');
    }
    final p = QazVoice.internals.platform;
    // Claim the engine before the model loads, so no session starts meanwhile.
    final placeholder = _session = SttSession._(this);
    try {
      return await use(() async {
        final wav = await readWavInfo(file);
        var reported = 0;
        void report(double fraction, List<RawResult> finals, double base) {
          onProgress?.call(base + (1 - base) * fraction);
          for (final r in finals) {
            reported++;
            onResult?.call(_final(r));
          }
        }

        List<SttResult> done(List<RawResult> all) {
          // The tail flushed at the end of the file was not reported yet.
          for (final r in all.skip(reported)) {
            onResult?.call(_final(r));
          }
          onProgress?.call(1);
          return [for (final x in all) _final(x)];
        }

        if (wav != null) {
          final r = await _backend.transcribePcmFile(file.path,
              rate: wav.sampleRate,
              channels: wav.channels,
              offset: wav.dataOffset,
              length: wav.dataLength,
              onProgress: (f, finals) => report(f, finals, 0),
              cancelToken: cancelToken);
          return done(r);
        }
        const decodeShare = 0.2;
        final token = ++_decodeToken;
        final tmp = File(
            '${await p.tempDir()}/qazvoice_decode_${DateTime.now().microsecondsSinceEpoch}.pcm');
        void onCancel() => unawaited(p.cancelDecode(token));
        cancelToken?.addListener(onCancel);
        final sub = QazVoice.internals.events
            .where((e) => e['type'] == 'decode' && e['token'] == token)
            .listen((e) => onProgress?.call(
                decodeShare * ((e['fraction'] as num?)?.toDouble() ?? 0)));
        try {
          final info = await p.decode(file.path, tmp.path, token);
          await sub.cancel();
          if (cancelToken?.isCancelled ?? false) throw const QazCancelled();
          final r = await _backend.transcribePcmFile(tmp.path,
              rate: (info['rate'] as num).toInt(),
              channels: (info['channels'] as num).toInt(),
              onProgress: (f, finals) => report(f, finals, decodeShare),
              cancelToken: cancelToken);
          return done(r);
        } finally {
          cancelToken?.removeListener(onCancel);
          await sub.cancel();
          if (await tmp.exists()) await tmp.delete();
        }
      });
    } finally {
      if (identical(_session, placeholder)) {
        _session = null;
        touch();
      }
    }
  }

  static SttResult _final(RawResult r) => SttResult(
      text: r.text, isFinal: true, startMs: r.startMs, endMs: r.endMs);
}

class _Listen {
  _Listen(this.session);
  final SttSession session;
  StreamSubscription<Uint8List>? sub;
  Future<void> pending = Future.value();
  bool stopping = false;
}

/// One recognition stream. Feed audio with [addPcm16] / [addFloat32] (or let
/// [QazStt.listen] do it), read [results], then [finish].
class SttSession {
  SttSession._(this._stt);

  final QazStt _stt;
  final _results = StreamController<SttResult>.broadcast();
  final _events = StreamController<SttEvent>.broadcast();
  final _level = StreamController<double>.broadcast();
  final List<SttResult> _finals = [];
  bool _closed = false;
  bool _speaking = false;
  bool _heardSpeech = false;
  Timer? _partialTimer;
  bool _partialBusy = false;
  int _utterance = 0;
  String _lastInterim = '';
  Future<void> _feeding = Future.value();

  /// Interim and final results.
  Stream<SttResult> get results => _results.stream;

  /// Speech start / end.
  Stream<SttEvent> get events => _events.stream;

  /// Input loudness 0..1 per chunk.
  Stream<double> get level => _level.stream;

  /// Finals so far.
  List<SttResult> get finals => List.unmodifiable(_finals);

  /// All final text so far, joined with spaces.
  String get text => _finals.map((r) => r.text).join(' ');

  /// Whether someone is speaking now (an utterance is open).
  bool get isSpeaking => _speaking;

  bool get isClosed => _closed;

  /// Pushes interleaved 16-bit PCM at any sample rate / channel count.
  Future<void> addPcm16(Int16List data,
      {int sampleRate = 16000, int channels = 1}) {
    _checkOpen();
    if (!_level.isClosed) _level.add(pcmLevel(data));
    return _enqueue(() => _stt._backend.feed16(data, sampleRate, channels));
  }

  /// Pushes interleaved float PCM in [-1, 1].
  Future<void> addFloat32(Float32List data,
      {int sampleRate = 16000, int channels = 1}) {
    _checkOpen();
    return _enqueue(() => _stt._backend.feed32(data, sampleRate, channels));
  }

  Future<void> _enqueue(Future<FeedResult> Function() feed) {
    final f = _feeding.then((_) => _stt.use(() async {
          if (_closed) return;
          _handle(await feed());
        }));
    // A failed chunk must not block the ones after it.
    _feeding = f.catchError((Object _) {});
    return f;
  }

  void _handle(FeedResult r) {
    for (final f in r.finals) {
      _emitFinal(f);
    }
    if (r.speaking != _speaking) {
      _speaking = r.speaking;
      if (_speaking) {
        _heardSpeech = true;
        _events.add(SttEvent.speechStart);
        _partialTimer ??= Timer.periodic(
            const Duration(milliseconds: 700), (_) => _partial());
      } else {
        _events.add(SttEvent.speechEnd);
        _partialTimer?.cancel();
        _partialTimer = null;
      }
    }
  }

  void _emitFinal(RawResult f) {
    _utterance++;
    _lastInterim = '';
    final r = QazStt._final(f);
    _finals.add(r);
    if (!_results.isClosed) _results.add(r);
  }

  Future<void> _partial() async {
    if (_partialBusy || !_speaking || _closed) return;
    _partialBusy = true;
    final u = _utterance;
    try {
      final p = await _stt.use(() => _stt._backend.partial());
      if (p == null || _closed || u != _utterance || !_speaking) return;
      if (p.text == _lastInterim) return;
      _lastInterim = p.text;
      _results.add(SttResult(
          text: p.text, isFinal: false, startMs: p.startMs, endMs: p.endMs));
    } catch (e) {
      QazVoice.internals.log(QazLogLevel.warning, 'interim result failed: $e');
    } finally {
      _partialBusy = false;
    }
  }

  /// Ends the stream: flushes the last utterance, closes the streams and
  /// returns every final result.
  Future<List<SttResult>> finish() async {
    _checkOpen();
    await _feeding;
    _partialTimer?.cancel();
    try {
      final tail = await _stt.use(() => _stt._backend.finish());
      for (final f in tail) {
        _emitFinal(f);
      }
      if (_speaking) {
        _speaking = false;
        _events.add(SttEvent.speechEnd);
      }
    } finally {
      await _close();
    }
    return finals;
  }

  /// Drops buffered audio and starts over (same session, streams stay open).
  Future<void> reset() async {
    _checkOpen();
    await _feeding;
    await _stt.use(() => _stt._backend.reset());
    _speaking = false;
    _partialTimer?.cancel();
    _partialTimer = null;
    _utterance++;
  }

  /// Abandons the session without results.
  Future<void> cancel() async {
    if (_closed) return;
    _closed = true;
    _partialTimer?.cancel();
    try {
      await _feeding;
    } catch (_) {}
    if (_stt.isLoaded && !_stt.isClosed) {
      try {
        await _stt._backend.reset();
      } catch (_) {}
    }
    await _close(force: true);
  }

  void _fail(Object e) {
    if (!_results.isClosed) _results.addError(e);
  }

  void _checkOpen() {
    if (_closed) throw StateError('session is finished');
  }

  Future<void> _close({bool force = false}) async {
    _closed = true;
    _partialTimer?.cancel();
    // A session that the microphone feeds: closing it ends listening too.
    final l = _stt._listen;
    if (l != null && identical(l.session, this) && !l.stopping) {
      unawaited(_stt.stopListening());
    }
    if (identical(_stt._session, this)) {
      _stt._session = null;
      _stt.touch();
    }
    await Future.wait([_results.close(), _events.close(), _level.close()]);
  }
}
