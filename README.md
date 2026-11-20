# qazvoice

On-device **Kazakh text-to-speech and speech recognition** for Flutter.
Everything runs on the phone. After a one-time model download, no network is needed.

- **TTS**: 6 natural voices (Piper VITS, 22 kHz). Streams sentence by sentence and
  starts speaking after the first one. Built-in playback with sentence
  highlighting and a level meter. Export to WAV or M4A.
- **STT**: FastConformer transducer with Silero VAD. Live microphone with interim
  results. Optional punctuation, numbers as digits, Latin output and hotwords.
  Transcribes audio and video files (wav, mp3, m4a, ogg, flac, mp4…).
- **Text tools**: Cyrillic ⇄ Latin (2021 alphabet), number words ⇄ digits,
  sentence splitting and punctuation restoration.
- **Model manager**: downloads run in a background service. They are resumable,
  verified with SHA-256, retried and failed over between mirrors, and can be
  paused, resumed or cancelled. Progress shows in a notification and they
  continue after the app is swiped away.
- **Never blocks the UI**: model loading and inference run on background isolates.

Platforms: **Android** (arm64-v8a, armeabi-v7a, x86_64; Android 7.0+) and **iOS** (15.1+, arm64).
You do not need Rust or any native toolchain: the native libraries ship in the package.

## Install

```yaml
dependencies:
  qazvoice: ^0.0.1
```

To use the microphone (`QazStt.listen`), add this to `android/app/src/main/AndroidManifest.xml`:

```xml
<uses-permission android:name="android.permission.RECORD_AUDIO" />
```

The plugin already declares the permissions its downloader needs (`INTERNET`,
`FOREGROUND_SERVICE_DATA_SYNC`, `POST_NOTIFICATIONS`).

On iOS the minimum deployment target is **15.1** (set `platform :ios, '15.1'` in
`ios/Podfile` if you use CocoaPods). Add a microphone usage text to `ios/Runner/Info.plist` (needed for `listen`):

```xml
<key>NSMicrophoneUsageDescription</key>
<string>Speech recognition uses the microphone.</string>
```

On iOS, downloads use a background `URLSession`. They continue while the app is
suspended, and the system finishes them even if it terminates the app (not if
the user force-quits it). iOS shows no download notification; use the
`state`/`progress` streams in your UI.

## Models

Models are not bundled with your app. Each one is downloaded on demand from
[eq-devs/qazvoice-models](https://github.com/eq-devs/qazvoice-models) releases:

| id | used by | download | on disk | RAM when loaded |
|---|---|---|---|---|
| `tts-kk` | `QazTts` | 119 MB | 128 MB | ~400 MB |
| `stt-kk` | `QazStt` | 153 MB | 196 MB | ~600 MB |
| `punct-kk` | punctuation (optional) | 75 MB | 114 MB | ~300 MB |
| `stt-kk-giga` | `QazStt(model: highAccuracyModelId)` (optional) | 253 MB | 322 MB | ~500 MB |

Large files are downloaded zstd-compressed and unpacked in the background; both the
download and the unpacked file are checked against SHA-256.

Accuracy on FLEURS Kazakh read speech (word error rate): `stt-kk` 9.3%,
`stt-kk-giga` 7.8% (about 1.6× slower). Measured on a Pixel 8 Pro: `stt-kk` runs about
29× faster than real time; the voice speaks after about
0.8 s and synthesizes 1.1–1.6× faster than real time (slower when the phone is hot,
and on lower-end phones).

## Quick start

```dart
import 'package:qazvoice/qazvoice.dart';

await QazVoice.init();

// Download once (in the background, with a notification).
final voice = QazVoice.model('tts-kk');
if (!voice.isInstalled) await voice.download();

// Speak.
final tts = await QazTts.open();
await tts.speak('Сәлеметсіз бе! Бүгін күн ашық.', speaker: 2);

// Recognize speech from the microphone.
await QazVoice.model('stt-kk').download();
final stt = await QazStt.open(numbersToDigits: true);
final session = await stt.listen(autoStopAfter: const Duration(seconds: 2));
session.results.listen((r) => print('${r.isFinal ? 'final' : '…'} ${r.text}'));
```

## Model manager

```dart
final m = QazVoice.model('stt-kk');

// UI: rebuild on every state change.
ValueListenableBuilder<ModelState>(
  valueListenable: m.state,
  builder: (context, s, _) => switch (s) {
    NotInstalled(:final partialBytes) => Text('Not installed'),
    Downloading(:final progress) => LinearProgressIndicator(value: progress.fraction),
    Waiting(:final reason) => Text('Waiting: ${reason.name}'), // queued / network / wifi
    Paused(:final progress) => Text('Paused at ${progress.received} bytes'),
    Verifying() => const Text('Verifying…'),
    Installed(:final version) => Text('Installed v$version'),
    Failed(:final error) => Text('Failed: ${error.message}'),
  },
);

await m.download(wifiOnly: true);   // completes when installed; same Future if called twice
await m.pause();  await m.resume();  await m.cancel();  await m.delete();
m.progress;          // Stream<DownloadProgress>: received, total, fraction, bytesPerSecond, eta
m.downloadSize;  m.installedSize;  m.updateAvailable;  m.version;

final check = await QazVoice.checkDevice('stt-kk'); // CPU ABI, RAM, free space
if (!check.ok) print(check.problems);
await QazVoice.storageUsed;
QazVoice.attributions;  // license notices to show in your app (required by CC BY 4.0)
```

How downloads work:

- They run in a foreground service (`dataSync`) with a progress notification
  that has Pause / Cancel buttons. They keep going when the app is in the
  background or swiped away. If the process is killed, they resume when the
  service or the app starts again.
- Partial files are kept. A resumed download continues with HTTP `Range` /
  `If-Range`, and starts the file over if the server changed it.
- Every file is checked against its SHA-256 while streaming. Only verified files
  are installed, and the install is atomic.
- Errors are retried with exponential backoff, then the next mirror is tried.
  With no network (or `wifiOnly` on mobile data) the download waits and
  continues by itself when the network is back.
- Free space is checked before and during the download.

Notification texts can be localized: `QazVoice.init(notification: const QazNotificationConfig.kk())`
uses Kazakh, or pass your own strings.

Your own mirror (for example a company server), with headers that are sent only to that host:

```dart
await QazVoice.init(mirrors: [
  QazMirror('https://models.example.com/qazvoice/{id}/{version}/{file}',
      headers: {'Authorization': 'Bearer $key'}),
]);
```

## Text-to-speech

```dart
final tts = await QazTts.open(threads: 0, idleTimeout: const Duration(minutes: 3),
    cacheBytes: 8 << 20); // repeated sentences come from this cache
tts.speakers;    // 6 voices; pass the index as `speaker:`
tts.sampleRate;  // 22050

// Built-in playback.
await tts.speak(text, speaker: 0, speed: 1.0, sentencePause: const Duration(milliseconds: 250));
await tts.stop();
tts.speaking;    // ValueListenable<bool>
tts.current;     // ValueListenable<TtsChunk?>: the sentence being heard (start/end offsets for highlighting)
tts.level;       // Stream<double> 0..1 for a level meter

// Raw audio.
await for (final chunk in tts.synthesize(text)) { /* chunk.pcm: Int16List */ }
final pcm = await tts.synthesizeAll(text);
final wavBytes = QazTts.toWav(pcm, tts.sampleRate);
await tts.saveAudio('/path/out.m4a', text, format: QazAudioFormat.m4a);

await tts.preload();  // load now instead of on first use
await tts.close();
```

The model loads on first use and is released after `idleTimeout`, or when
Android reports memory pressure. The next call loads it again.

## Speech recognition

```dart
final stt = await QazStt.open(
  model: QazStt.modelId,    // or QazStt.highAccuracyModelId (GigaAM, more accurate)
  punctuation: true,        // needs the punct-kk model
  numbersToDigits: true,    // "жиырма бес" → "25"
  latin: false,             // output in the 2021 Latin alphabet
  hotwords: ['Алматы', 'Qazvoice'],
  minSilence: const Duration(milliseconds: 600),
);

// Microphone.
final session = await stt.listen(autoStopAfter: const Duration(seconds: 2));
session.results;  // Stream<SttResult>: interim (isFinal == false) and final results
session.events;   // speechStart / speechEnd
session.level;    // input level 0..1
await stt.stopListening();

// Your own audio source.
final s = stt.session();
await s.addPcm16(int16Samples, sampleRate: 48000, channels: 2);
await s.addFloat32(floatSamples, sampleRate: 16000);
final finals = await s.finish();

// Files: wav, mp3, m4a, ogg, flac, video soundtracks…
final token = QazCancelToken();
final results = await stt.transcribeFile(path,
    onProgress: (p) => print(p), onResult: (r) => print(r.text), cancelToken: token);

await stt.setHotwords(['Нұр-Сұлтан']);
```

## Text tools

```dart
QazText.toLatin('Қазақстан');            // Qazaqstan
QazText.toCyrillic('Qazaqstan');         // Қазақстан
QazText.numbersToDigits('жиырма бес');   // 25
QazText.spellNumbers('25 адам');         // жиырма бес адам
QazText.split('Сәлем! Қалың қалай?');    // sentences with offsets
await QazText.punctuate('сәлем қалың қалай'); // needs punct-kk
```

## Errors

Every error is a `QazException` subtype: `QazModelNotInstalled`,
`QazInsufficientStorage`, `QazChecksumMismatch`, `QazNetworkError`,
`QazUnsupportedDevice`, `QazPermissionDenied`, `QazCancelled` and `QazEngineError`.

## Testing your app

`QazVoice.useMock()` swaps in fake engines and a fake platform: no native code,
no network. Use it in widget and unit tests.

```dart
QazVoice.useMock(platform: MockQazPlatform(installed: {'tts-kk': 1}));
await QazVoice.init();
```

## Example

[`example/`](example) is a complete app with a Kazakh UI. It covers the model
manager, the TTS screen (voices, speed, highlighting, saving and sharing m4a),
the STT screen (live microphone, toggles, file transcription) and the text tools.

## Licenses

The plugin is Apache-2.0. The models have their own licenses. **CC BY 4.0
requires attribution**, so show `QazVoice.attributions` somewhere in your app:

- `tts-kk`: Piper `kk_KZ-issai-high`, trained on data from Mussakhojayeva et al. (2021),
  "KazakhTTS: An Open-Source Kazakh Text-to-Speech Synthesis Dataset", Interspeech 2021,
  ISSAI, Nazarbayev University. CC BY 4.0.
- `stt-kk`: NVIDIA `stt_kk_ru_fastconformer_hybrid_large`, CC BY 4.0; Silero VAD, MIT.
- `punct-kk`: `1-800-BAD-CODE/xlm-roberta_punctuation_fullstop_truecase`, Apache-2.0.
- `stt-kk-giga`: GigaAM Multilingual by Sber AI, MIT; Silero VAD, MIT.

ONNX Runtime (MIT) comes from the `com.microsoft.onnxruntime:onnxruntime-android` package.

## Building the native library

Plugin users never need this step. To change the Rust code in `rust/`, rebuild
the libraries with `tool/build_android.sh`, which needs Rust, cargo-ndk and NDK r28+.
`tool/host_lib.sh` builds a desktop copy for `test/native_test.dart`.
