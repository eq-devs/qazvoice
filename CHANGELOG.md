## 0.0.4

- More accurate recognition: the default model's word error rate on FLEURS Kazakh
  drops from 10.3% to 9.3%. The upstream NVIDIA tokenizer sometimes produces
  corrupted pieces ("тағы басқаиғи" for "табиғи", "басқаасшысы" for "басшысы");
  these are now repaired in post-processing.
- Optional high-accuracy recognizer `stt-kk-giga` (GigaAM Multilingual, MIT):
  `QazStt.open(model: QazStt.highAccuracyModelId)`. On FLEURS Kazakh: 7.8% WER vs
  9.3%, about 1.6× slower (still 36× faster than real time on the emulator),
  253 MB download. The default model is unchanged.
- TTS look-ahead: synthesis keeps running up to 30 s ahead of playback, so short
  slow-downs no longer cause gaps between sentences.
- TTS cache: recently synthesized sentences are reused (`QazTts.open(cacheBytes:)`,
  8 MB by default), so repeated prompts play instantly.
- Attribution for the voice now cites the KazakhTTS paper, as ISSAI requests.

## 0.0.3

- Smaller downloads: large model files are fetched as zstd copies and unpacked in
  the background (Android download service / iOS download queue, never the UI
  thread). Both the compressed file and the unpacked result are checked against
  SHA-256. Downloads: speech recognition 196 → 153 MB, punctuation 114 → 75 MB,
  voice 128 → 119 MB.
- `QazModel.downloadSize` is now the bytes downloaded; new `QazModel.diskSize` is
  the installed size. `checkDevice` includes room for unpacking.
- The voice runs once while loading (warm-up), so the first sentence starts sooner.

## 0.0.2

- iOS support (iOS 15.1+, arm64 devices and Apple-silicon simulators), same Dart API:
  background `URLSession` downloads (continue while the app is suspended or was
  terminated by the system; resume data kept on pause), SHA-256 verification,
  `AVAudioEngine` playback and microphone capture, `AVAssetReader` decoding of
  audio/video files, AAC (.m4a) export, memory-warning release.
  The native engine ships as a dynamic framework inside the package (Swift Package
  Manager and CocoaPods both supported).

## 0.0.1

First release (Android).

- `QazTts`: on-device Kazakh speech synthesis with 6 voices, streaming by sentence,
  built-in playback with sentence highlighting and level, WAV / M4A export.
- `QazStt`: on-device recognition from the microphone (interim + final results,
  voice activity events, auto-stop), from PCM you feed, and from audio/video files.
  Optional punctuation, numbers as digits, Latin output, hotwords.
- `QazText`: Cyrillic ⇄ Latin, number words ⇄ digits, sentence split, punctuation.
- Model manager: background downloads in a foreground service with a notification,
  resumable, SHA-256 verified, retries with backoff, mirror failover, pause / resume /
  cancel, Wi-Fi only, free-space checks, re-attach after restart.
- `QazVoice.useMock()` for tests.
