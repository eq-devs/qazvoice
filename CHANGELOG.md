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
