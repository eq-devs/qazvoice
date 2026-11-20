import 'dart:async';
import 'dart:io';
import 'dart:math' as math;
import 'dart:typed_data';

import 'package:flutter_test/flutter_test.dart';
import 'package:qazvoice/qazvoice.dart';
import 'package:qazvoice/src/audio.dart';

Future<T> waitFor<T>(Stream<T> s, bool Function(T) test,
        {Duration timeout = const Duration(seconds: 5)}) =>
    s.firstWhere(test).timeout(timeout);

Future<void> until(bool Function() test,
    {Duration timeout = const Duration(seconds: 5)}) async {
  final end = DateTime.now().add(timeout);
  while (!test()) {
    if (DateTime.now().isAfter(end)) {
      throw TimeoutException('condition not met');
    }
    await Future<void>.delayed(const Duration(milliseconds: 5));
  }
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  late MockQazPlatform platform;

  Future<void> setUpMock(
      {Map<String, int> installed = const {}, int freeBytes = 8 << 30}) async {
    platform = MockQazPlatform(installed: installed, freeBytes: freeBytes);
    QazVoice.useMock(platform: platform);
    await QazVoice.init(fetchCatalog: false);
  }

  group('models', () {
    setUp(() => setUpMock());

    test('init lists catalog models, all not installed', () {
      expect(QazVoice.isMock, isTrue);
      expect(QazVoice.models.map((m) => m.id),
          containsAll(['tts-kk', 'stt-kk', 'punct-kk']));
      final m = QazVoice.model('tts-kk');
      expect(m.state.value, const NotInstalled());
      expect(m.isInstalled, isFalse);
      expect(m.downloadSize, greaterThan(100 << 20));
      expect(() => QazVoice.model('nope'), throwsArgumentError);
      expect(QazVoice.attributions.first.text, isNotEmpty);
      expect(QazVoice.version, '0.0.4');
    });

    test('download goes through progress to installed', () async {
      final m = QazVoice.model('tts-kk');
      final states = <ModelState>[];
      m.state.addListener(() => states.add(m.state.value));
      final progress = <DownloadProgress>[];
      final sub = m.progress.listen(progress.add);
      await m.download();
      await sub.cancel();
      expect(m.isInstalled, isTrue);
      expect(m.installedVersion, 1);
      expect(states.whereType<Downloading>(), isNotEmpty);
      expect(states.whereType<Verifying>(), hasLength(1));
      expect(states.last, isA<Installed>());
      expect(progress.first.fraction, lessThan(progress.last.fraction));
      expect(progress.last.eta, isNotNull);
      // Downloading an installed model is a no-op.
      await m.download();
      expect(await QazVoice.storageUsed, m.diskSize);
    });

    test('concurrent download calls share one future and one job', () async {
      final m = QazVoice.model('stt-kk');
      final a = m.download();
      final b = m.download();
      expect(identical(a, b), isTrue);
      await Future.wait([a, b]);
      expect(m.isInstalled, isTrue);
    });

    test('pause keeps progress, resume finishes', () async {
      final m = QazVoice.model('tts-kk');
      final done = m.download();
      await until(() => m.state.value is Downloading);
      await m.pause();
      expect(m.state.value, isA<Paused>());
      final p = (m.state.value as Paused).progress;
      expect(p.received, greaterThan(0));
      await m.resume();
      await done;
      expect(m.isInstalled, isTrue);
    });

    test('cancel throws QazCancelled and resets', () async {
      final m = QazVoice.model('tts-kk');
      final done = m.download();
      await until(() => m.state.value is Downloading);
      await m.cancel();
      await expectLater(done, throwsA(isA<QazCancelled>()));
      expect(m.state.value, const NotInstalled());
    });

    test('failures surface as typed errors and Failed state', () async {
      final m = QazVoice.model('punct-kk');
      platform.failNext['punct-kk'] = const QazChecksumMismatch('model.onnx');
      await expectLater(m.download(), throwsA(isA<QazChecksumMismatch>()));
      expect(m.state.value, isA<Failed>());
      expect((m.state.value as Failed).error, isA<QazChecksumMismatch>());
      // Retrying works.
      await m.download();
      expect(m.isInstalled, isTrue);
    });

    test('delete returns to NotInstalled and closes engines', () async {
      final m = QazVoice.model('tts-kk');
      await m.download();
      final tts = await QazTts.open();
      await tts.preload();
      await m.delete();
      expect(m.state.value, const NotInstalled());
      expect(tts.isClosed, isTrue);
      expect(() => QazTts.open(), throwsA(isA<QazModelNotInstalled>()));
    });

    test('device check', () async {
      final d = await QazVoice.checkDevice('tts-kk');
      expect(d.ok, isTrue, reason: '${d.problems}');
      final m = QazVoice.model('tts-kk');
      // Download plus room to unpack the compressed voice next to it.
      expect(d.requiredBytes, m.downloadSize + m.spec.files.first.size);
    });
  });

  test('insufficient storage', () async {
    await setUpMock(freeBytes: 1000);
    final d = await QazVoice.checkDevice('tts-kk');
    expect(d.enoughStorage, isFalse);
    await expectLater(
        QazVoice.model('tts-kk').download(),
        throwsA(isA<QazInsufficientStorage>()
            .having((e) => e.neededBytes, 'needed', greaterThan(1000))));
  });

  test('engines refuse to open without their model', () async {
    await setUpMock();
    expect(() => QazTts.open(), throwsA(isA<QazModelNotInstalled>()));
    expect(() => QazStt.open(), throwsA(isA<QazModelNotInstalled>()));
    await QazVoice.model('stt-kk').download();
    expect(
        () => QazStt.open(punctuation: true),
        throwsA(isA<QazModelNotInstalled>()
            .having((e) => e.modelId, 'id', 'punct-kk')));
    expect(() => QazText.punctuate('x'), throwsA(isA<QazModelNotInstalled>()));
  });

  test('repeated sentences come from the cache; cache can be off', () async {
    var calls = 0;
    platform = MockQazPlatform(installed: {'tts-kk': 1});
    QazVoice.useMock(
        platform: platform, tts: () => _CountingTts(() => calls++));
    await QazVoice.init(fetchCatalog: false);
    final tts = await QazTts.open();
    await tts.synthesizeAll('Сәлем. Қалайсыз?');
    final first = calls;
    await tts.synthesizeAll('Сәлем. Қалайсыз?');
    expect(calls, first, reason: 'second time fully cached');
    await tts.synthesizeAll('Сәлем.', speed: 1.5);
    expect(calls, first + 1, reason: 'different speed is a different entry');
    await tts.close();
    final off = await QazTts.open(cacheBytes: 0);
    await off.preload(); // includes one warm-up synthesis
    final before = calls;
    await off.synthesizeAll('Сәлем.');
    await off.synthesizeAll('Сәлем.');
    expect(calls - before, 2);
    await off.close();
  });

  test('high-accuracy recognizer is selected by model id', () async {
    await setUpMock(installed: {'stt-kk-giga': 1});
    expect(() => QazStt.open(), throwsA(isA<QazModelNotInstalled>()));
    final stt = await QazStt.open(model: QazStt.highAccuracyModelId);
    expect(stt.model, 'stt-kk-giga');
    expect(QazVoice.model('stt-kk-giga').downloadSize,
        lessThan(QazVoice.model('stt-kk-giga').diskSize));
    await stt.close();
    expect(() => QazStt.open(model: 'tts-kk'), throwsA(anything));
  });

  group('tts', () {
    late QazTts tts;
    setUp(() async {
      await setUpMock(installed: {'tts-kk': 1});
      tts = await QazTts.open(idleTimeout: const Duration(milliseconds: 100));
    });
    tearDown(() => tts.close());

    test('loads lazily, synthesizes a chunk per sentence with spans', () async {
      expect(tts.isLoaded, isFalse);
      expect(tts.speakers, hasLength(6));
      const text = 'Сәлем! Қалың қалай?';
      final chunks = await tts.synthesize(text, speaker: 1).toList();
      expect(tts.isLoaded, isTrue);
      expect(chunks, hasLength(2));
      expect(text.substring(chunks[1].start, chunks[1].end), 'Қалың қалай?');
      expect(chunks[0].pauseSamples, tts.sampleRate ~/ 4);
      expect(chunks.map((c) => c.index), [0, 1]);
      final all = await tts.synthesizeAll(text);
      expect(all.length, chunks.fold<int>(0, (a, c) => a + c.pcm.length));
    });

    test('rejects bad arguments', () {
      expect(() => tts.synthesize('x', speaker: 9), throwsRangeError);
      expect(() => tts.synthesize('x', speed: 3), throwsRangeError);
    });

    test('idle timeout releases the model, next call reloads', () async {
      await tts.preload();
      expect(tts.isLoaded, isTrue);
      await until(() => !tts.isLoaded, timeout: const Duration(seconds: 2));
      await tts.synthesizeAll('Бір.');
      expect(tts.isLoaded, isTrue);
    });

    test('speak plays, reports current chunk and level, completes', () async {
      final seen = <String>[];
      tts.current.addListener(() {
        final c = tts.current.value;
        if (c != null && (seen.isEmpty || seen.last != c.text)) {
          seen.add(c.text);
        }
      });
      final levels = <double>[];
      final sub = tts.level.listen(levels.add);
      await tts.speak('Бірінші сөйлем. Екінші сөйлем.');
      await sub.cancel();
      expect(seen, ['Бірінші сөйлем.', 'Екінші сөйлем.']);
      expect(levels, isNotEmpty);
      expect(tts.isSpeaking, isFalse);
      expect(tts.current.value, isNull);
    });

    test('stop ends speak early; a new speak replaces the old', () async {
      final first = tts.speak('Ұзын мәтін. ' * 20);
      await until(() => tts.isSpeaking);
      await tts.stop();
      await first.timeout(const Duration(seconds: 2));
      expect(tts.isSpeaking, isFalse);
      final a = tts.speak('Бір. Екі. Үш.');
      await until(() => tts.isSpeaking);
      final b = tts.speak('Төрт.');
      await Future.wait([a, b]).timeout(const Duration(seconds: 5));
    });

    test('saveAudio writes a valid wav and an m4a', () async {
      final dir = await Directory.systemTemp.createTemp('qv');
      final wav = await tts.saveAudio('${dir.path}/a.wav', 'Сәлем.');
      final info = await readWavInfo(wav);
      expect(info!.sampleRate, 22050);
      expect(info.channels, 1);
      expect(info.dataLength, greaterThan(0));
      final m4a = await tts.saveAudio('${dir.path}/a.m4a', 'Сәлем.',
          format: QazAudioFormat.m4a);
      expect(await m4a.length(), greaterThan(0));
      await dir.delete(recursive: true);
    });

    test('memory pressure releases an idle model, not a speaking one',
        () async {
      await tts.preload();
      platform.emitTrimMemory();
      await until(() => !tts.isLoaded, timeout: const Duration(seconds: 2));
      final speaking = tts.speak('Бір. Екі. Үш. Төрт. Бес.');
      await until(() => tts.isSpeaking);
      platform.emitTrimMemory();
      await Future<void>.delayed(const Duration(milliseconds: 50));
      expect(tts.isLoaded, isTrue);
      await speaking;
    });
  });

  group('stt', () {
    late QazStt stt;
    setUp(() async {
      await setUpMock(installed: {'stt-kk': 1, 'punct-kk': 1});
      stt = await QazStt.open(punctuation: true);
    });
    tearDown(() => stt.close());

    Int16List tone(double secs, {bool loud = true}) =>
        Int16List.fromList(List.generate((16000 * secs).round(),
            (i) => loud ? (math.sin(i * 0.08) * 9000).round() : 0));

    test('session: events, interim and final results', () async {
      final s = stt.session();
      final results = <SttResult>[];
      final events = <SttEvent>[];
      s.results.listen(results.add);
      s.events.listen(events.add);
      final levels = <double>[];
      s.level.listen(levels.add);
      await s.addPcm16(tone(0.3, loud: false));
      await s.addPcm16(tone(1.0));
      await Future<void>.delayed(const Duration(milliseconds: 800));
      expect(s.isSpeaking, isTrue);
      await s.addPcm16(tone(0.8, loud: false));
      final finals = await s.finish();
      expect(events, [SttEvent.speechStart, SttEvent.speechEnd]);
      expect(finals.single.text, 'Сәлем әлем');
      expect(finals.single.startMs, inInclusiveRange(250, 350));
      expect(results.where((r) => !r.isFinal), isNotEmpty);
      expect(results.last.isFinal, isTrue);
      expect(levels.any((l) => l > 0.5), isTrue);
      expect(s.isClosed, isTrue);
      expect(() => s.addPcm16(tone(0.1)), throwsStateError);
    });

    test('float input and resampling path', () async {
      final s = stt.session();
      final f = Float32List.fromList(List.generate(
          48000, (i) => i > 8000 ? math.sin(i * 0.03) * 0.3 : 0.0));
      await s.addFloat32(f, sampleRate: 48000, channels: 1);
      final r = await s.finish();
      expect(r, hasLength(1));
    });

    test('listen records from the microphone and auto-stops after silence',
        () async {
      final session =
          await stt.listen(autoStopAfter: const Duration(milliseconds: 300));
      expect(stt.isListening, isTrue);
      final finals = <SttResult>[];
      session.results.where((r) => r.isFinal).listen(finals.add);
      await until(() => !stt.isListening, timeout: const Duration(seconds: 10));
      expect(finals.single.text, 'Сәлем әлем');
      expect(session.isClosed, isTrue);
    });

    test('listen without microphone permission', () async {
      platform.micAllowed = false;
      await expectLater(stt.listen(), throwsA(isA<QazPermissionDenied>()));
    });

    test('transcribeFile on a wav with progress; cancellable', () async {
      final dir = await Directory.systemTemp.createTemp('qv');
      final f = File('${dir.path}/a.wav')
        ..writeAsBytesSync(pcm16ToWav(tone(1), 16000));
      final progress = <double>[];
      final r = await stt.transcribeFile(f.path, onProgress: progress.add);
      expect(r.single.text, 'Сәлем әлем');
      expect(progress.last, 1.0);
      final token = QazCancelToken()..cancel();
      await expectLater(stt.transcribeFile(f.path, cancelToken: token),
          throwsA(isA<QazCancelled>()));
      await expectLater(stt.transcribeFile('${dir.path}/missing.wav'),
          throwsA(isA<QazEngineError>()));
      await dir.delete(recursive: true);
    });

    test('hotwords can change at run time', () async {
      await stt.preload();
      await stt.setHotwords(['Алматы']);
    });

    test('punctuate (mock model)', () async {
      expect(await QazText.punctuate('сәлем әлем'), 'Сәлем әлем.');
      await QazText.releasePunctuation();
    });

    test('closed engine refuses work', () async {
      await stt.close();
      expect(() => stt.session(), throwsA(isA<QazEngineError>()));
    });
  });
}

class _CountingTts extends MockTtsBackend {
  _CountingTts(this.onSynth);
  final void Function() onSynth;
  @override
  Future<Int16List> synth(String piece, int speaker, double speed) {
    onSynth();
    return super.synth(piece, speaker, speed);
  }
}
