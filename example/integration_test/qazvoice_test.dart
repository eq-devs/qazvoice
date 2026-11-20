// On-device end-to-end test with the real engine and real model downloads.
//
//   flutter test integration_test/qazvoice_test.dart \
//     [--dart-define=QAZVOICE_MIRROR=http://10.0.2.2:8765/{id}/{file}]
//
// Without QAZVOICE_MIRROR the models come from the eq-devs/qazvoice-models
// GitHub releases.
import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/scheduler.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:path_provider/path_provider.dart';
import 'package:qazvoice/qazvoice.dart';

/// Measures the longest stall of the UI isolate while [body] runs: a 16 ms
/// periodic timer that fires late means the isolate was busy.
Future<Duration> maxStall(Future<void> Function() body) async {
  var worst = Duration.zero;
  var last = DateTime.now();
  final t = Timer.periodic(const Duration(milliseconds: 16), (_) {
    final now = DateTime.now();
    final gap = now.difference(last);
    if (gap > worst) worst = gap;
    last = now;
  });
  try {
    await body();
  } finally {
    t.cancel();
  }
  return worst;
}

void main() {
  final binding = IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  const mirror = String.fromEnvironment('QAZVOICE_MIRROR');

  setUpAll(() async {
    await QazVoice.init(
      mirrors: [if (mirror.isNotEmpty) const QazMirror(mirror)],
      preferMirrors: mirror.isNotEmpty,
      notification: const QazNotificationConfig.kk(),
    );
  });

  testWidgets('catalog and device', (tester) async {
    expect(QazVoice.models.map((m) => m.id), containsAll(['tts-kk', 'stt-kk', 'punct-kk']));
    final d = await QazVoice.checkDevice('tts-kk');
    expect(d.supportedAbi, isTrue, reason: '${d.abis}');
    expect(d.nativeLibrary, isTrue);
    expect(d.totalRamBytes, greaterThan(1 << 30));
    expect(QazText.toLatin('Қазақстан'), 'Qazaqstan');
    expect(QazText.numbersToDigits('жиырма бес'), '25');
  });

  testWidgets('download tts-kk with pause / resume', (tester) async {
    final m = QazVoice.model('tts-kk');
    if (m.isInstalled) await m.delete();
    final states = <String>{};
    m.state.addListener(() => states.add(m.state.value.runtimeType.toString()));
    final progress = <DownloadProgress>[];
    final sub = m.progress.listen(progress.add);
    final done = m.download();
    // Pause once some bytes arrived, then resume.
    while (m.state.value is! Downloading ||
        (m.state.value as Downloading).progress.received < 5 << 20) {
      await Future<void>.delayed(const Duration(milliseconds: 50));
      if (m.isInstalled) break;
    }
    if (!m.isInstalled) {
      await m.pause();
      while (m.state.value is! Paused) {
        await Future<void>.delayed(const Duration(milliseconds: 20));
      }
      final at = (m.state.value as Paused).progress.received;
      expect(at, greaterThan(0));
      await m.resume();
    }
    await done.timeout(const Duration(minutes: 10));
    await sub.cancel();
    expect(m.isInstalled, isTrue);
    expect(states, containsAll(['Downloading', 'Installed']));
    expect(m.installedSize, m.downloadSize);
    // Progress updates are throttled to about 5 a second.
    if (progress.length > 20) {
      final span = progress.length;
      expect(span, lessThan(5 * 60 * 10));
    }
  });

  testWidgets('download stt-kk', (tester) async {
    final m = QazVoice.model('stt-kk');
    await m.download().timeout(const Duration(minutes: 10));
    expect(m.isInstalled, isTrue);
  });

  testWidgets('TTS: load and synthesize off the UI isolate; speak; save m4a', (tester) async {
    final tts = await QazTts.open();
    expect(tts.speakers, hasLength(6));
    expect(tts.sampleRate, 22050);
    late List<TtsChunk> chunks;
    const text = 'Сәлеметсіз бе! Бүгін ауа райы жақсы. 2024 жылы 15 мамырда.';
    final stall = await maxStall(() async {
      await tts.preload();
      chunks = await tts.synthesize(text, speaker: 3).toList();
    });
    expect(chunks, hasLength(3));
    expect(text.substring(chunks[2].start, chunks[2].end), '2024 жылы 15 мамырда.');
    expect(chunks.every((c) => c.pcm.length > 2000), isTrue);
    // ignore: avoid_print
    print('tts: max UI-isolate stall ${stall.inMilliseconds} ms');
    expect(stall, lessThan(const Duration(milliseconds: 100)));

    final seen = <int>{};
    tts.current.addListener(() {
      final c = tts.current.value;
      if (c != null) seen.add(c.index);
    });
    final t = Stopwatch()..start();
    await tts.speak('Бір. Екі. Үш.').timeout(const Duration(seconds: 30));
    expect(seen, containsAll([0, 1, 2]));
    expect(t.elapsed, greaterThan(const Duration(milliseconds: 800)));

    // stop() ends playback quickly.
    final long = tts.speak('Ұзын мәтін оқылады. ' * 10);
    while (!tts.isSpeaking) {
      await Future<void>.delayed(const Duration(milliseconds: 20));
    }
    await Future<void>.delayed(const Duration(milliseconds: 500));
    final s = Stopwatch()..start();
    await tts.stop();
    await long.timeout(const Duration(seconds: 3));
    expect(s.elapsed, lessThan(const Duration(seconds: 1)));

    final dir = await getTemporaryDirectory();
    final m4a = await tts.saveAudio('${dir.path}/it.m4a', 'Алматы қаласында жиырма бес мың адам тұрады.',
        format: QazAudioFormat.m4a, speaker: 3);
    expect(await m4a.length(), greaterThan(5000));
    final wav = await tts.saveAudio('${dir.path}/it.wav', 'Алматы қаласында жиырма бес мың адам тұрады.',
        speaker: 3);
    expect(await wav.length(), greaterThan(50000));
    await tts.close();
  });

  testWidgets('STT: wav and m4a (platform decoder), progress, cancel', (tester) async {
    final dir = await getTemporaryDirectory();
    final stt = await QazStt.open(numbersToDigits: true);
    late List<SttResult> r;
    final progress = <double>[];
    final stall = await maxStall(() async {
      await stt.preload();
      r = await stt.transcribeFile('${dir.path}/it.wav', onProgress: progress.add);
    });
    final text = r.map((x) => x.text).join(' ');
    // ignore: avoid_print
    print('stt wav: "$text", max UI stall ${stall.inMilliseconds} ms');
    expect(text.toLowerCase(), contains('алматы'));
    expect(text, contains('25'));
    expect(progress.last, 1.0);
    expect(stall, lessThan(const Duration(milliseconds: 100)));

    final m4a = await stt.transcribeFile('${dir.path}/it.m4a', onProgress: progress.add);
    final t2 = m4a.map((x) => x.text).join(' ');
    // ignore: avoid_print
    print('stt m4a: "$t2"');
    expect(t2.toLowerCase(), contains('алматы'));

    final token = QazCancelToken();
    final f = stt.transcribeFile('${dir.path}/it.m4a', cancelToken: token);
    token.cancel();
    await expectLater(f, throwsA(isA<QazCancelled>()));

    // A live session fed from the file in 100 ms chunks gives interim results.
    final bytes = await File('${dir.path}/it.wav').readAsBytes();
    final pcm = bytes.buffer.asInt16List(44);
    final s = stt.session();
    final interim = <String>[];
    s.results.where((x) => !x.isFinal).listen((x) => interim.add(x.text));
    for (var i = 0; i < pcm.length; i += 2205) {
      await s.addPcm16(pcm.sublist(i, i + 2205 > pcm.length ? pcm.length : i + 2205),
          sampleRate: 22050);
      await Future<void>.delayed(const Duration(milliseconds: 100)); // real time
    }
    final live = await s.finish();
    expect(live.map((x) => x.text).join(' ').toLowerCase(), contains('алматы'));
    expect(interim, isNotEmpty);
    await stt.close();
  });

  testWidgets('frames stay smooth while a model loads and synthesizes', (tester) async {
    binding.framePolicy = LiveTestWidgetsFlutterBindingFramePolicy.fullyLive;
    final timings = <FrameTiming>[];
    void onTimings(List<FrameTiming> t) => timings.addAll(t);
    SchedulerBinding.instance.addTimingsCallback(onTimings);
    await tester.pumpWidget(const MaterialApp(
        home: Scaffold(body: Center(child: CircularProgressIndicator()))));
    final tts = await QazTts.open();
    final work = () async {
      await tts.preload();
      await tts.synthesizeAll('Бұл кадрларды тексеруге арналған ұзынырақ сөйлем. Тағы бір сөйлем.');
    }();
    while (!tts.isLoaded) {
      await tester.pump(const Duration(milliseconds: 16));
    }
    await work;
    await tester.pump(const Duration(milliseconds: 500));
    SchedulerBinding.instance.removeTimingsCallback(onTimings);
    binding.framePolicy = LiveTestWidgetsFlutterBindingFramePolicy.fadePointers;
    // The first frames build the widget tree from scratch (JIT in debug): skip them.
    final build = timings.skip(3).map((t) => t.buildDuration.inMicroseconds).toList();
    // ignore: avoid_print
    print('frames: ${timings.length}, first ${timings.take(3).map((t) => t.buildDuration.inMilliseconds).toList()} ms, '
        'slowest after: ${(List.of(build)..sort()).reversed.take(3).map((u) => u ~/ 1000).toList()} ms');
    build.sort();
    final worst = build.isEmpty ? 0 : build.last;
    expect(worst, lessThan(16000 * 3), reason: 'UI thread blocked during model work');
    await tts.close();
  });

  testWidgets('delete frees storage', (tester) async {
    final before = await QazVoice.storageUsed;
    await QazVoice.model('stt-kk').delete();
    final after = await QazVoice.storageUsed;
    expect(after, lessThan(before - (150 << 20)));
    expect(QazVoice.model('stt-kk').state.value, const NotInstalled());
    expect(() => QazStt.open(), throwsA(isA<QazModelNotInstalled>()));
  });
}
