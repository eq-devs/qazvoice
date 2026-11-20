// Stress tests on a device: repeated and overlapping calls, open/close loops,
// memory growth. Run after qazvoice_test.dart has installed the models, or
// let setUpAll download them.
//
//   flutter test integration_test/stress_test.dart [--dart-define=QAZVOICE_MIRROR=…]
import 'dart:async';
import 'dart:io';
import 'dart:math';

import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:path_provider/path_provider.dart';
import 'package:qazvoice/qazvoice.dart';

int rssMb() => ProcessInfo.currentRss ~/ (1 << 20);

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  const mirror = String.fromEnvironment('QAZVOICE_MIRROR');
  final rnd = Random(7);

  setUpAll(() async {
    await QazVoice.init(
      mirrors: [if (mirror.isNotEmpty) const QazMirror(mirror)],
      preferMirrors: mirror.isNotEmpty,
    );
    await Future.wait([
      QazVoice.model('tts-kk').download(),
      QazVoice.model('stt-kk').download(),
    ]).timeout(const Duration(minutes: 15));
  });

  testWidgets('speak / stop storm: never hangs, always ends idle', (t) async {
    final tts = await QazTts.open();
    await tts.preload();
    for (var i = 0; i < 25; i++) {
      final f = tts.speak('Бірінші сөйлем. Екінші сөйлем. Үшінші сөйлем.',
          speaker: rnd.nextInt(6), speed: 0.8 + rnd.nextDouble() * 0.8);
      await Future<void>.delayed(Duration(milliseconds: rnd.nextInt(400)));
      if (rnd.nextBool()) {
        await tts.stop();
      } else if (rnd.nextBool()) {
        // A new speak replaces the running one.
        unawaited(tts.speak('Жаңа мәтін.'));
      }
      await f.timeout(const Duration(seconds: 20));
    }
    await tts.stop();
    await Future<void>.delayed(const Duration(milliseconds: 300));
    expect(tts.isSpeaking, isFalse);
    expect(tts.current.value, isNull);
    await tts.close();
  }, timeout: const Timeout(Duration(minutes: 5)));

  testWidgets('concurrent synthesize streams on one engine', (t) async {
    final tts = await QazTts.open();
    final results = await Future.wait([
      for (var i = 0; i < 4; i++)
        tts.synthesizeAll('Сөйлем нөмір ${i + 1}. Тағы бір сөйлем.'),
    ]).timeout(const Duration(minutes: 2));
    for (final r in results) {
      expect(r.length, greaterThan(tts.sampleRate)); // > 1 s each
    }
    // Cancelling a stream mid-way leaves the engine usable.
    final sub = tts.synthesize('Бір. Екі. Үш. Төрт. Бес.').listen((_) {});
    await Future<void>.delayed(const Duration(milliseconds: 200));
    await sub.cancel();
    expect((await tts.synthesizeAll('Жұмыс істейді.')).length, greaterThan(0));
    await tts.close();
  }, timeout: const Timeout(Duration(minutes: 5)));

  testWidgets('open / close loops do not leak memory', (t) async {
    final dir = await getTemporaryDirectory();
    QazTts? tts = await QazTts.open();
    await tts.saveAudio(
        '${dir.path}/s.wav', 'Алматы қаласында жиырма бес мың адам тұрады.');
    await tts.close();
    final start = <int>[];
    for (var i = 0; i < 6; i++) {
      tts = await QazTts.open();
      await tts.synthesizeAll('Сәлем әлем.');
      await tts.close();
      final stt = await QazStt.open();
      final r = await stt.transcribeFile('${dir.path}/s.wav');
      expect(r.map((x) => x.text).join(' ').toLowerCase(), contains('алматы'));
      await stt.close();
      start.add(rssMb());
    }
    // ignore: avoid_print
    print('rss after each open/close round (MB): $start');
    // After warm-up rounds the resident size must not keep climbing.
    expect(start.last - start[2], lessThan(120), reason: '$start');
  }, timeout: const Timeout(Duration(minutes: 8)));

  testWidgets('session churn and file cancel loops', (t) async {
    final dir = await getTemporaryDirectory();
    if (!File('${dir.path}/s.wav').existsSync()) {
      final tts = await QazTts.open();
      await tts.saveAudio(
          '${dir.path}/s.wav', 'Алматы қаласында жиырма бес мың адам тұрады.');
      await tts.close();
    }
    final stt = await QazStt.open(numbersToDigits: true);
    final pcm =
        (await File('${dir.path}/s.wav').readAsBytes()).buffer.asInt16List(22);
    // Rapid sessions: each new one cancels the previous; the last one must work.
    SttSession? s;
    for (var i = 0; i < 15; i++) {
      s = stt.session();
      await s.addPcm16(pcm.sublist(0, 4410), sampleRate: 22050);
    }
    for (var i = 0; i < pcm.length; i += 4410) {
      await s!.addPcm16(pcm.sublist(i, min(i + 4410, pcm.length)),
          sampleRate: 22050);
    }
    final finals = await s!.finish();
    expect(
        finals.map((x) => x.text).join(' ').toLowerCase(), contains('алматы'));
    // Cancel file transcription at random points, then a full run still works.
    for (var i = 0; i < 6; i++) {
      final tok = QazCancelToken();
      final f = stt.transcribeFile('${dir.path}/s.wav', cancelToken: tok);
      Future<void>.delayed(
          Duration(milliseconds: rnd.nextInt(300)), tok.cancel);
      try {
        await f;
      } on QazCancelled {
        // expected most of the time
      }
    }
    final r = await stt.transcribeFile('${dir.path}/s.wav');
    expect(r.map((x) => x.text).join(' '), contains('25000'));
    // listen() then immediately stop, repeatedly (emulator mic gives silence).
    for (var i = 0; i < 5; i++) {
      try {
        final ls = await stt.listen();
        await Future<void>.delayed(
            Duration(milliseconds: 100 + rnd.nextInt(300)));
        if (i.isEven) {
          await stt.stopListening();
        } else {
          await ls.cancel(); // closing the session must stop the mic too
        }
        await Future<void>.delayed(const Duration(milliseconds: 200));
        expect(stt.isListening, isFalse);
        // ignore: avoid_print
        print('listen cycle ${i + 1} ok');
      } on QazPermissionDenied {
        // CI emulators without a granted microphone: skip this part.
        break;
      }
    }
    await stt.close();
  }, timeout: const Timeout(Duration(minutes: 5)));

  testWidgets('memory pressure while idle, then work again', (t) async {
    final tts =
        await QazTts.open(idleTimeout: const Duration(milliseconds: 300));
    await tts.preload();
    await Future<void>.delayed(const Duration(seconds: 1));
    expect(tts.isLoaded, isFalse,
        reason: 'idle timeout should release the model');
    final pcm = await tts.synthesizeAll('Қайта жүктелді.');
    expect(pcm.length, greaterThan(0));
    await tts.close();
    // A closed engine refuses work with a clear error.
    expect(() => tts.synthesize('x'), throwsA(isA<QazEngineError>()));
  });
}
