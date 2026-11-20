import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';

import 'package:flutter/foundation.dart';

import 'audio.dart';
import 'backends.dart';
import 'engine.dart';
import 'errors.dart';
import 'voice.dart';

/// Output formats of [QazTts.saveAudio].
enum QazAudioFormat {
  /// 16-bit PCM WAV.
  wav,

  /// AAC in an MPEG-4 container (small; good for sharing).
  m4a,
}

/// A synthesized piece of speech, tied to the sentence it reads.
@immutable
class TtsChunk {
  const TtsChunk({
    required this.pcm,
    required this.index,
    required this.text,
    required this.start,
    required this.end,
    required this.sampleRate,
    required this.pauseSamples,
  });

  /// 16-bit mono PCM at [sampleRate], including the pause after the piece.
  final Int16List pcm;

  /// 0-based position in the stream.
  final int index;

  /// The source sentence as written (several chunks may share one).
  final String text;

  /// UTF-16 span of [text] in the input string (`input.substring(start, end)`).
  final int start, end;
  final int sampleRate;

  /// Trailing silence included in [pcm].
  final int pauseSamples;

  Duration get duration =>
      Duration(microseconds: pcm.length * 1000000 ~/ sampleRate);

  @override
  String toString() => 'TtsChunk($index, "$text", $duration)';
}

/// On-device Kazakh text-to-speech (model `tts-kk`).
///
/// ```dart
/// final tts = await QazTts.open();
/// await tts.speak('Сәлеметсіз бе! Бүгін күн ашық.', speaker: 2);
/// ```
///
/// Model loading and synthesis run on a background isolate. One [QazTts]
/// speaks one text at a time; [speak] stops the previous one.
class QazTts extends ModelEngine {
  QazTts._(this._backend, this._voicePath, this.sampleRate, this.speakers,
      this._threads, Duration? idleTimeout)
      : super(idleTimeout);

  /// Model id used by this engine.
  static const modelId = 'tts-kk';

  final TtsBackend _backend;
  final String _voicePath;
  final int _threads;

  /// Output sample rate (22050 for `tts-kk`).
  final int sampleRate;

  /// Speaker names in id order (`speaker:` index into this list).
  final List<String> speakers;

  final ValueNotifier<bool> _speaking = ValueNotifier(false);
  final ValueNotifier<TtsChunk?> _current = ValueNotifier(null);
  final StreamController<double> _level = StreamController.broadcast();
  _Speech? _speech;

  /// Opens the voice. Throws [QazModelNotInstalled] if `tts-kk` is not
  /// installed. The model itself loads on first use (or with [preload]).
  ///
  /// [threads] 0 = choose by CPU count. [idleTimeout] frees the model's
  /// memory after that long without use (it reloads on the next call).
  static Future<QazTts> open({
    int threads = 0,
    Duration? idleTimeout = const Duration(minutes: 3),
    bool preload = false,
  }) async {
    final internals = QazVoice.internals;
    await internals.ensureInit();
    final model = internals.model(modelId)..ensureInstalled();
    final path = '${model.directory}/voice.onnx';
    var rate = 22050;
    var speakers = const <String>[];
    final json = File('$path.json');
    if (!internals.isMock || await json.exists()) {
      try {
        final cfg = jsonDecode(await json.readAsString()) as Map;
        rate =
            ((cfg['audio'] as Map?)?['sample_rate'] as num?)?.toInt() ?? rate;
        final map = (cfg['speaker_id_map'] as Map?) ?? const {};
        final byId = map.entries.toList()
          ..sort((a, b) => (a.value as num).compareTo(b.value as num));
        speakers = [for (final e in byId) '${e.key}'];
      } on FileSystemException {
        throw const QazModelNotInstalled(modelId);
      } on FormatException {
        throw const QazEngineError('voice.onnx.json is not valid JSON',
            kind: 'model');
      }
    } else {
      speakers = const ['F1', 'F2', 'F3', 'M1', 'M2', 'M3'];
    }
    final t = threads > 0 ? threads : Platform.numberOfProcessors.clamp(2, 4);
    final tts = QazTts._(internals.newTts(), path, rate,
        List.unmodifiable(speakers), t, idleTimeout);
    internals.register(tts);
    if (preload) await tts.preload();
    return tts;
  }

  @override
  Iterable<String> get modelIds => const [modelId];

  @override
  bool get isLoaded => _backend.isLoaded;

  @override
  bool get pinned => _speech != null;

  @override
  Future<void> doLoad() => _backend.load(_voicePath, _threads);

  @override
  Future<void> doUnload() => _backend.unload();

  @override
  Future<void> doClose() async {
    await stop();
    await _backend.dispose();
    await _level.close();
  }

  /// Whether [speak] is playing now.
  bool get isSpeaking => _speaking.value;

  /// True while [speak] plays.
  ValueListenable<bool> get speaking => _speaking;

  /// The chunk being heard during [speak] (null otherwise); use its
  /// `start`/`end` to highlight the sentence.
  ValueListenable<TtsChunk?> get current => _current;

  /// Output loudness 0..1 while [speak] plays (~20 updates a second).
  Stream<double> get level => _level.stream;

  /// Synthesizes [text] sentence by sentence. The next piece is synthesized
  /// while the consumer handles the current one. Cancel the subscription to stop.
  ///
  /// [speaker] indexes [speakers]; [speed] 1.0 is natural (0.5–2.0);
  /// [sentencePause] is the silence after each sentence.
  Stream<TtsChunk> synthesize(String text,
      {int speaker = 0,
      double speed = 1.0,
      Duration sentencePause = const Duration(milliseconds: 250)}) {
    checkOpen();
    _checkArgs(speaker, speed);
    late StreamController<TtsChunk> c;
    var cancelled = false;
    Future<void> run() async {
      try {
        await use(() async {
          final plan = await _backend.plan(text);
          final pause = sentencePause.inMicroseconds * sampleRate ~/ 1000000;
          Future<Int16List>? next = plan.isEmpty
              ? null
              : _backend.synth(plan[0].piece, speaker, speed);
          var index = 0;
          for (var i = 0; i < plan.length; i++) {
            final pcm = await next!;
            if (cancelled) return;
            next = i + 1 < plan.length
                ? _backend.synth(plan[i + 1].piece, speaker, speed)
                : null;
            next?.ignore();
            if (pcm.isEmpty) continue;
            final p = plan[i];
            final tail = p.pause ? pause : 0;
            final out = Int16List(pcm.length + tail)..setAll(0, pcm);
            c.add(TtsChunk(
                pcm: out,
                index: index++,
                text: p.sentence,
                start: p.start,
                end: p.end,
                sampleRate: sampleRate,
                pauseSamples: tail));
            // Wait while the listener is paused (back-pressure).
            while (c.isPaused && !cancelled) {
              await Future<void>.delayed(const Duration(milliseconds: 10));
            }
            if (cancelled) {
              await next?.catchError((_) => Int16List(0));
              return;
            }
          }
        });
      } catch (e, st) {
        if (!cancelled) c.addError(e, st);
      } finally {
        if (!c.isClosed) await c.close();
      }
    }

    c = StreamController<TtsChunk>(
      onListen: () => unawaited(run()),
      onCancel: () => cancelled = true,
    );
    return c.stream;
  }

  /// The whole text as one PCM buffer (pauses included).
  Future<Int16List> synthesizeAll(String text,
      {int speaker = 0,
      double speed = 1.0,
      Duration sentencePause = const Duration(milliseconds: 250)}) async {
    final parts = await synthesize(text,
            speaker: speaker, speed: speed, sentencePause: sentencePause)
        .toList();
    final n = parts.fold<int>(0, (a, c) => a + c.pcm.length);
    final out = Int16List(n);
    var o = 0;
    for (final c in parts) {
      out.setAll(o, c.pcm);
      o += c.pcm.length;
    }
    return out;
  }

  /// 16-bit mono PCM → WAV bytes.
  static Uint8List toWav(Int16List pcm, int sampleRate) =>
      pcm16ToWav(pcm, sampleRate);

  /// Synthesizes [text] into a file at [path] ([QazAudioFormat.wav] or
  /// [QazAudioFormat.m4a]) and returns it.
  Future<File> saveAudio(String path, String text,
      {QazAudioFormat format = QazAudioFormat.wav,
      int speaker = 0,
      double speed = 1.0,
      Duration sentencePause = const Duration(milliseconds: 250)}) async {
    final pcm = await synthesizeAll(text,
        speaker: speaker, speed: speed, sentencePause: sentencePause);
    final out = File(path);
    await out.parent.create(recursive: true);
    switch (format) {
      case QazAudioFormat.wav:
        await out.writeAsBytes(pcm16ToWav(pcm, sampleRate), flush: true);
      case QazAudioFormat.m4a:
        final p = QazVoice.internals.platform;
        final tmp = File(
            '${await p.tempDir()}/qazvoice_${DateTime.now().microsecondsSinceEpoch}.pcm');
        try {
          await tmp.writeAsBytes(pcm16ToBytes(pcm), flush: true);
          await p.encodeM4a(tmp.path, sampleRate, out.path);
        } finally {
          if (await tmp.exists()) await tmp.delete();
        }
    }
    return out;
  }

  /// Reads [text] aloud through the speaker and completes when done (or
  /// stopped). Playback starts after the first sentence is synthesized.
  /// [current] and [level] follow playback.
  Future<void> speak(String text,
      {int speaker = 0,
      double speed = 1.0,
      Duration sentencePause = const Duration(milliseconds: 250)}) async {
    checkOpen();
    _checkArgs(speaker, speed);
    // Claim the slot synchronously, so concurrent speak() calls each stop the
    // one before them and only the newest keeps playing.
    final previous = _speech;
    final s = _speech = _Speech();
    _speaking.value = true;
    if (previous != null) await _stopSpeech(previous);
    final p = QazVoice.internals.platform;
    var finished = false;
    final chunks = <(int, TtsChunk)>[];
    StreamSubscription<Map<String, Object?>>? sub;
    StreamSubscription<TtsChunk>? synth;
    try {
      if (s.stopped) return;
      s.player = await p.playerCreate(sampleRate);
      if (s.stopped) return;
      sub = QazVoice.internals.events
          .where((e) => e['type'] == 'player' && e['id'] == s.player)
          .listen((e) {
        final frames = (e['frames'] as num).toInt();
        final lv = (e['level'] as num?)?.toDouble() ?? 0;
        if (!_level.isClosed) _level.add(lv);
        TtsChunk? now;
        for (final (offset, c) in chunks) {
          if (offset <= frames) now = c;
        }
        if (now != null && !identical(now, _current.value)) {
          _current.value = now;
        }
      });
      var offset = 0;
      final done = Completer<void>();
      synth = synthesize(text,
              speaker: speaker, speed: speed, sentencePause: sentencePause)
          .listen((c) {
        chunks.add((offset, c));
        offset += c.pcm.length;
        _current.value ??= c;
        synth!.pause(p
            .playerWrite(s.player!, pcm16ToBytes(c.pcm))
            .catchError((Object e) {
          if (!done.isCompleted) done.completeError(e);
        }));
      }, onError: (Object e) {
        if (!done.isCompleted) done.completeError(e);
      }, onDone: () {
        if (!done.isCompleted) done.complete();
      });
      s.synth = synth;
      await Future.any([done.future, s.stoppedFuture]);
      if (!s.stopped) {
        await Future.any([
          p.playerFinish(s.player!).then((_) => finished = true),
          s.stoppedFuture
        ]);
      }
    } finally {
      await synth?.cancel();
      await sub?.cancel();
      // Stopped, failed or still playing: release the AudioTrack and audio focus.
      if (s.player != null && !finished) {
        await p.playerStop(s.player!).catchError((Object _) {});
      }
      if (identical(_speech, s)) {
        _speech = null;
        _speaking.value = false;
        _current.value = null;
        if (!_level.isClosed) _level.add(0);
        touch();
      }
    }
  }

  /// Stops [speak] at once.
  Future<void> stop() async {
    final s = _speech;
    if (s != null) await _stopSpeech(s);
  }

  Future<void> _stopSpeech(_Speech s) async {
    s.stop();
    await s.synth?.cancel();
    if (s.player != null) {
      await QazVoice.internals.platform.playerStop(s.player!);
    }
  }

  void _checkArgs(int speaker, double speed) {
    if (speaker < 0 || (speakers.isNotEmpty && speaker >= speakers.length)) {
      throw RangeError.range(speaker, 0, speakers.length - 1, 'speaker');
    }
    if (!(speed >= 0.5 && speed <= 2.0)) {
      throw RangeError.value(speed, 'speed', 'must be within 0.5–2.0');
    }
  }
}

class _Speech {
  int? player;
  StreamSubscription<TtsChunk>? synth;
  bool stopped = false;
  final Completer<void> _stopped = Completer();
  Future<void> get stoppedFuture => _stopped.future;
  void stop() {
    stopped = true;
    if (!_stopped.isCompleted) _stopped.complete();
  }
}
