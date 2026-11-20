// Compares the default and the high-accuracy recognizer on the device:
// download, load time, speed, memory, and the transcript of the same audio.
//
//   flutter test integration_test/models_bench_test.dart
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:path_provider/path_provider.dart';
import 'package:qazvoice/qazvoice.dart';

int rssMb() => ProcessInfo.currentRss ~/ (1 << 20);

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  testWidgets('default vs high-accuracy recognizer', (t) async {
    await QazVoice.init();
    for (final id in ['tts-kk', 'stt-kk', 'stt-kk-giga']) {
      final m = QazVoice.model(id);
      final sw = Stopwatch()..start();
      await m.download().timeout(const Duration(minutes: 20));
      // ignore: avoid_print
      print(
          '$id: download ${m.downloadSize ~/ 1000000} MB, disk ${m.installedSize ~/ 1000000} MB, '
          '${sw.elapsed.inSeconds} s');
    }
    final dir = await getTemporaryDirectory();
    final wav = '${dir.path}/bench.wav';
    final tts = await QazTts.open();
    const text =
        'Қазақстан Республикасы Орталық Азияда орналасқан. Елдің астанасы Астана қаласы, '
        'ең үлкен қаласы Алматы. Халқының саны жиырма миллионнан асады. Ресми тілі қазақ тілі. '
        'Елде көптеген табиғи ресурстар бар: мұнай, газ, уран және көмір. '
        'Қазақстанның аумағы әлемдегі ең үлкен тоғызыншы аумақ болып саналады.';
    await tts.saveAudio(wav, text, speaker: 3);
    await tts.close();
    final audioSecs = (await File(wav).length() - 44) / 2 / 22050;

    for (final id in [QazStt.modelId, QazStt.highAccuracyModelId]) {
      final rss0 = rssMb();
      final load = Stopwatch()..start();
      final stt = await QazStt.open(model: id, numbersToDigits: true);
      await stt.preload();
      final loadMs = load.elapsedMilliseconds;
      final run = Stopwatch()..start();
      final r = await stt.transcribeFile(wav);
      final secs = run.elapsedMilliseconds / 1000;
      final text = r.map((x) => x.text).join(' ');
      // ignore: avoid_print
      print(
          '$id: load $loadMs ms, ${(audioSecs / secs).toStringAsFixed(1)}x real time '
          '(${audioSecs.toStringAsFixed(1)} s audio in ${secs.toStringAsFixed(2)} s), '
          'memory +${rssMb() - rss0} MB\n  "$text"');
      expect(text.toLowerCase(), contains('алматы'));
      await stt.close();
    }
  }, timeout: const Timeout(Duration(minutes: 30)));
}
