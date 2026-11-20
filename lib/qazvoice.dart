/// On-device Kazakh text-to-speech and speech recognition.
///
/// Start with [QazVoice.init], download models with [QazModel.download], then
/// use [QazTts], [QazStt] and [QazText].
library;

export 'src/backends.dart'
    show
        TtsBackend,
        SttBackend,
        PunctBackend,
        MockTtsBackend,
        MockSttBackend,
        MockPunctBackend,
        PlanItem,
        TtsInfo,
        RawResult,
        FeedResult;
export 'src/catalog.dart' show QazMirror, QazModelKind, ModelSpec, ModelFile;
export 'src/errors.dart';
export 'src/platform.dart' show MockQazPlatform;
export 'src/stt.dart';
export 'src/text.dart';
export 'src/tts.dart';
export 'src/voice.dart' hide QazInternals, QazEngineHolder;
