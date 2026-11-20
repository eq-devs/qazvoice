// Runs the real native engine (host build of rust/) through the public API.
//
//   cargo build --release -p qazvoice-ffi   (in rust/; macOS needs the clang_rt
//   link arg, see tool/host_lib.sh)
//   QAZVOICE_LIB=rust/target/host/release/libqazvoice.dylib \
//   QAZVOICE_MODELS=/path/with/tts-kk,stt-kk,punct-kk flutter test test/native_test.dart
//
// Skipped when QAZVOICE_LIB is not set.
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:qazvoice/qazvoice.dart';
import 'package:qazvoice/src/backends.dart';
import 'package:qazvoice/src/native/ffi.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  final lib = Platform.environment['QAZVOICE_LIB'];
  final models = Platform.environment['QAZVOICE_MODELS'];
  final skipLib = lib == null ? 'QAZVOICE_LIB not set' : null;
  final skipModels = skipLib ?? (models == null ? 'QAZVOICE_MODELS not set' : null);

  setUpAll(() {
    if (lib != null) QvLib.overridePath = lib;
  });

  group('QazText (native)', () {
    test('transliteration both ways', () {
      // 2021 alphabet as implemented by the engine: И/Й → ı, І → i.
      final latin = QazText.toLatin('Қазақстан Республикасы біз');
      expect(latin, 'Qazaqstan Respublıkasy biz');
      expect(QazText.toCyrillic(latin), 'Қазақстан Республикасы біз');
    });
    test('numbers', () {
      expect(QazText.numbersToDigits('жиырма бес адам'), '25 адам');
      expect(QazText.spellNumbers('25 адам'), 'жиырма бес адам');
    });
    test('split with spans', () {
      const t = 'Сәлем! Қалың қалай? т.б. және';
      final s = QazText.split(t);
      expect(s.map((x) => x.text), ['Сәлем!', 'Қалың қалай? т.б. және']);
      expect(t.substring(s[1].start, s[1].end), s[1].text);
    });
    test('odd input does not crash', () {
      expect(QazText.toLatin(''), '');
      expect(QazText.toLatin('a\u0000b'), isA<String>());
      expect(QazText.split('😀😀. ' * 1000), hasLength(1000));
    });
  }, skip: skipLib);

  group('engines with real models', () {
    setUpAll(() async {
      QazVoice.useMock(
        platform: MockQazPlatform(
            modelsDir: models, installed: {'tts-kk': 1, 'stt-kk': 1, 'punct-kk': 1}),
        tts: NativeTtsBackend.new,
        stt: NativeSttBackend.new,
        punct: NativePunctBackend.new,
      );
      await QazVoice.init(fetchCatalog: false);
    });

    test('TTS → WAV → STT round trip, punctuation', () async {
      final tts = await QazTts.open(preload: true);
      expect(tts.speakers, hasLength(6));
      expect(tts.sampleRate, 22050);
      final dir = await Directory.systemTemp.createTemp('qv');
      const text = 'Бүгін ауа райы жақсы. Біз саябаққа барамыз.';
      final chunks = await tts.synthesize(text, speaker: 2).toList();
      expect(chunks, hasLength(2));
      expect(text.substring(chunks[1].start, chunks[1].end), 'Біз саябаққа барамыз.');
      final wav = await tts.saveAudio('${dir.path}/a.wav', text);
      await tts.close();

      final stt = await QazStt.open(punctuation: true, numbersToDigits: true);
      final finals = <SttResult>[];
      final progress = <double>[];
      final r = await stt.transcribeFile(wav.path, onProgress: progress.add, onResult: finals.add);
      final all = r.map((x) => x.text).join(' ');
      // ignore: avoid_print
      print('transcript: $all');
      for (final w in ['бүгін', 'ауа', 'райы', 'саябаққа']) {
        expect(all.toLowerCase(), contains(w));
      }
      expect(all, contains('.'));
      expect(progress.last, 1.0);
      expect(finals, r);

      // The same audio fed as a live session in 100 ms chunks.
      final pcm = await File(wav.path).readAsBytes();
      final s = stt.session();
      final events = <SttEvent>[];
      s.events.listen(events.add);
      final samples = pcm.buffer.asInt16List(44);
      for (var i = 0; i < samples.length; i += 2205) {
        final end = i + 2205 > samples.length ? samples.length : i + 2205;
        await s.addPcm16(samples.sublist(i, end), sampleRate: 22050);
      }
      final live = await s.finish();
      expect(live.map((x) => x.text).join(' ').toLowerCase(), contains('саябаққа'));
      expect(events.first, SttEvent.speechStart);

      expect(await QazText.punctuate('сәлем қалың қалай'), startsWith('Сәлем'));
      await stt.close();
      await QazText.releasePunctuation();
      await dir.delete(recursive: true);
    }, timeout: const Timeout(Duration(minutes: 2)));
  }, skip: skipModels);
}
