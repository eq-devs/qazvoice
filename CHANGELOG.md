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
