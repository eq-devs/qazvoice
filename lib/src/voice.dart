import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';

import 'backends.dart';
import 'catalog.dart';
import 'default_catalog.dart';
import 'errors.dart';
import 'native/ffi.dart';
import 'platform.dart';

/// Version of this plugin.
const String qazvoicePluginVersion = '0.0.1';

/// Why a download is not moving right now.
enum WaitReason {
  /// Another model is downloading first.
  queued,

  /// No network connection; continues by itself when one is back.
  network,

  /// `wifiOnly` download on a metered network; continues on Wi-Fi.
  wifi,
}

/// Download progress of one model (all its files together).
@immutable
class DownloadProgress {
  const DownloadProgress({
    required this.received,
    required this.total,
    this.bytesPerSecond = 0,
  });

  /// Bytes on disk so far.
  final int received;

  /// Bytes of the whole model.
  final int total;

  /// Current speed (smoothed); 0 when unknown or not downloading.
  final double bytesPerSecond;

  /// 0..1.
  double get fraction => total <= 0 ? 0 : (received / total).clamp(0.0, 1.0);

  /// Estimated time left, or null when the speed is unknown.
  Duration? get eta => bytesPerSecond <= 0
      ? null
      : Duration(seconds: ((total - received) / bytesPerSecond).ceil());

  @override
  bool operator ==(Object other) =>
      other is DownloadProgress &&
      other.received == received &&
      other.total == total &&
      other.bytesPerSecond == bytesPerSecond;

  @override
  int get hashCode => Object.hash(received, total, bytesPerSecond);

  @override
  String toString() =>
      'DownloadProgress($received/$total, ${bytesPerSecond.round()} B/s)';
}

/// Where a model is in its life cycle. Use a `switch` to handle every case.
@immutable
sealed class ModelState {
  const ModelState();
}

/// Not on the device. [partialBytes] of an interrupted download are kept and
/// reused by the next `download()`.
final class NotInstalled extends ModelState {
  const NotInstalled({this.partialBytes = 0});
  final int partialBytes;
  @override
  bool operator ==(Object other) =>
      other is NotInstalled && other.partialBytes == partialBytes;
  @override
  int get hashCode => partialBytes.hashCode;
  @override
  String toString() => 'NotInstalled($partialBytes)';
}

/// Bytes are arriving.
final class Downloading extends ModelState {
  const Downloading(this.progress);
  final DownloadProgress progress;
  @override
  bool operator ==(Object other) =>
      other is Downloading && other.progress == progress;
  @override
  int get hashCode => progress.hashCode;
  @override
  String toString() => 'Downloading($progress)';
}

/// Will continue by itself (see [reason]).
final class Waiting extends ModelState {
  const Waiting(this.reason, this.progress);
  final WaitReason reason;
  final DownloadProgress progress;
  @override
  bool operator ==(Object other) =>
      other is Waiting && other.reason == reason && other.progress == progress;
  @override
  int get hashCode => Object.hash(reason, progress);
  @override
  String toString() => 'Waiting(${reason.name}, $progress)';
}

/// Paused by the user (app or notification). `resume()` continues.
final class Paused extends ModelState {
  const Paused(this.progress);
  final DownloadProgress progress;
  @override
  bool operator ==(Object other) =>
      other is Paused && other.progress == progress;
  @override
  int get hashCode => progress.hashCode;
  @override
  String toString() => 'Paused($progress)';
}

/// All bytes are here; checking SHA-256 and installing.
final class Verifying extends ModelState {
  const Verifying();
  @override
  bool operator ==(Object other) => other is Verifying;
  @override
  int get hashCode => 1;
  @override
  String toString() => 'Verifying()';
}

/// Ready to use.
final class Installed extends ModelState {
  const Installed({required this.version, required this.sizeBytes});
  final int version;
  final int sizeBytes;
  @override
  bool operator ==(Object other) =>
      other is Installed &&
      other.version == version &&
      other.sizeBytes == sizeBytes;
  @override
  int get hashCode => Object.hash(version, sizeBytes);
  @override
  String toString() => 'Installed(v$version, $sizeBytes)';
}

/// The last download failed. `download()` tries again (keeping verified bytes).
final class Failed extends ModelState {
  const Failed(this.error, {this.partialBytes = 0});
  final QazException error;
  final int partialBytes;
  @override
  bool operator ==(Object other) =>
      other is Failed &&
      other.error.message == error.message &&
      other.partialBytes == partialBytes;
  @override
  int get hashCode => Object.hash(error.message, partialBytes);
  @override
  String toString() => 'Failed($error)';
}

/// Texts of the Android download notification. Placeholders: `{name}` (model
/// name), `{done}` / `{total}` (sizes), `{speed}`, `{eta}`.
@immutable
class QazNotificationConfig {
  const QazNotificationConfig({
    this.channelName = 'Model downloads',
    this.downloading = 'Downloading {name}',
    this.progress = '{done} of {total} · {speed}',
    this.waitingNetwork = 'Waiting for network',
    this.waitingWifi = 'Waiting for Wi-Fi',
    this.paused = '{name}: paused',
    this.done = '{name} is ready',
    this.failed = '{name}: download failed',
    this.pause = 'Pause',
    this.resume = 'Resume',
    this.cancel = 'Cancel',
    this.retry = 'Retry',
    this.units = 'B,KB,MB,GB',
    this.perSecond = '/s',
    this.language = 'en',
  });

  /// Kazakh texts.
  const QazNotificationConfig.kk()
      : this(
          channelName: 'Модельдерді жүктеу',
          downloading: '{name} жүктелуде',
          progress: '{done} / {total} · {speed}',
          waitingNetwork: 'Желі күтілуде',
          waitingWifi: 'Wi-Fi күтілуде',
          paused: '{name}: кідіртілді',
          done: '{name} дайын',
          failed: '{name}: жүктеу сәтсіз',
          pause: 'Кідірту',
          resume: 'Жалғастыру',
          cancel: 'Бас тарту',
          retry: 'Қайталау',
          units: 'Б,КБ,МБ,ГБ',
          perSecond: '/с',
          language: 'kk',
        );

  final String channelName, downloading, progress, waitingNetwork, waitingWifi;
  final String paused, done, failed, pause, resume, cancel, retry;

  /// Size units, comma-separated: bytes, KB, MB, GB.
  final String units;

  /// Speed suffix, e.g. `/s`.
  final String perSecond;

  /// Language of the model names in the notification (`en`, `kk`).
  final String language;

  Map<String, Object?> toMap() => {
        'channelName': channelName,
        'downloading': downloading,
        'progress': progress,
        'waitingNetwork': waitingNetwork,
        'waitingWifi': waitingWifi,
        'paused': paused,
        'done': done,
        'failed': failed,
        'pause': pause,
        'resume': resume,
        'cancel': cancel,
        'retry': retry,
        'units': units,
        'perSecond': perSecond,
      };
}

/// Can this device run a model? See `QazVoice.checkDevice`.
@immutable
class DeviceCheck {
  const DeviceCheck({
    required this.modelId,
    required this.abis,
    required this.supportedAbi,
    required this.totalRamBytes,
    required this.availableRamBytes,
    required this.freeBytes,
    required this.requiredBytes,
    required this.requiredRamBytes,
    required this.nativeLibrary,
  });

  final String modelId;

  /// CPU ABIs of the device, preferred first.
  final List<String> abis;

  /// Whether one of [abis] has a native library in the plugin.
  final bool supportedAbi;
  final int totalRamBytes, availableRamBytes;

  /// Free storage where models are kept.
  final int freeBytes;

  /// Bytes still to download (0 if installed).
  final int requiredBytes;

  /// Memory the model needs when loaded (approximate).
  final int requiredRamBytes;

  /// Whether the native engine library loaded.
  final bool nativeLibrary;

  bool get enoughStorage => freeBytes >= requiredBytes;
  bool get enoughRam =>
      requiredRamBytes == 0 || totalRamBytes >= requiredRamBytes * 2;

  /// Everything needed to download and run the model.
  bool get ok => supportedAbi && nativeLibrary && enoughStorage && enoughRam;

  /// Human-readable problems (empty when [ok]).
  List<String> get problems => [
        if (!supportedAbi) 'unsupported CPU (${abis.join(', ')})',
        if (!nativeLibrary) 'native library failed to load',
        if (!enoughStorage)
          'not enough storage: $requiredBytes bytes needed, $freeBytes free',
        if (!enoughRam) 'not enough memory: $totalRamBytes bytes total',
      ];

  @override
  String toString() => 'DeviceCheck($modelId, ok: $ok, $problems)';
}

/// License notice for a model, to show on an About / licenses screen.
@immutable
class QazAttribution {
  const QazAttribution(
      {required this.modelId,
      required this.name,
      required this.license,
      required this.text});
  final String modelId, name, license, text;
}

/// Log levels of [QazLog].
enum QazLogLevel { debug, info, warning, error }

/// One log record from the plugin (see `QazVoice.onLog`).
@immutable
class QazLog {
  const QazLog(this.level, this.message, this.time);
  final QazLogLevel level;
  final String message;
  final DateTime time;
  @override
  String toString() => '[qazvoice ${level.name}] $message';
}

/// Something that holds a loaded model and can let go of it.
abstract class QazEngineHolder {
  /// Model ids this engine uses.
  Iterable<String> get modelIds;

  /// Frees the model memory (it is loaded again on next use) if idle.
  Future<void> releaseIfIdle();

  /// Closes the engine because its model is being deleted or replaced.
  Future<void> closeForModelChange();
}

/// A downloadable model (voice, recognizer, punctuation).
class QazModel {
  QazModel._(this._spec, this._dir);

  ModelSpec _spec;
  final String _dir;
  final ValueNotifier<ModelState> _state = ValueNotifier(const NotInstalled());
  final ValueNotifier<int> _catalogTick = ValueNotifier(0);
  final StreamController<DownloadProgress> _progress =
      StreamController.broadcast();
  Completer<void>? _pending;
  int? _installedVersion;
  int _installedBytes = 0;

  /// Catalog id, for example `tts-kk`.
  String get id => _spec.id;

  QazModelKind get kind => _spec.kind;

  /// Display name in [lang] (`en`, `kk`).
  String name([String lang = 'en']) => _spec.name(lang);

  /// Catalog version (what `download()` installs).
  int get version => _spec.version;

  /// Installed version, or null.
  int? get installedVersion => _installedVersion;

  /// SPDX license of the model files.
  String get license => _spec.license;

  /// Attribution text to show in the app.
  String get attribution => _spec.attribution;

  /// The catalog entry.
  ModelSpec get spec => _spec;

  /// Directory with the model files (valid once installed).
  String get directory => '$_dir/$id';

  /// Current state; listen to it to update UI.
  ValueListenable<ModelState> get state => _state;

  /// Notifies on state changes and when a newer catalog arrives (version,
  /// [updateAvailable]). Handy for `ListenableBuilder`.
  Listenable get changes => Listenable.merge([_state, _catalogTick]);

  /// Progress while downloading, at most 5 updates a second.
  Stream<DownloadProgress> get progress => _progress.stream;

  /// Bytes `download()` fetches in total.
  int get downloadSize => _spec.size;

  /// Bytes on disk when installed, else 0.
  int get installedSize => _installedBytes;

  bool get isInstalled => _state.value is Installed;

  /// Installed, but the catalog has a newer version.
  bool get updateAvailable =>
      _installedVersion != null && _installedVersion! < _spec.version;

  /// Downloads (or updates) the model in a background service and completes
  /// when it is installed. Calling it again while a download runs returns the
  /// same future. Throws [QazInsufficientStorage], [QazNetworkError],
  /// [QazChecksumMismatch] or [QazCancelled] (after `cancel()`).
  ///
  /// With [wifiOnly] the download waits on metered networks.
  Future<void> download({bool wifiOnly = false}) {
    if (isInstalled && !updateAvailable) return Future.value();
    final p = _pending;
    if (p != null && !p.isCompleted) {
      if (_state.value is Paused) unawaited(resume());
      return p.future;
    }
    final c = _pending = Completer<void>();
    c.future.ignore();
    QazVoice._instance._startDownload(this, wifiOnly).catchError((Object e) {
      if (!c.isCompleted) c.completeError(e);
    });
    return c.future;
  }

  /// Pauses a running download; downloaded bytes are kept.
  Future<void> pause() => QazVoice._instance._platform.pause(id);

  /// Continues a paused download.
  Future<void> resume() async {
    if (_state.value is! Paused && _state.value is! Failed) return;
    await QazVoice._instance._platform.resume(id);
  }

  /// Stops the download and deletes downloaded bytes. A pending `download()`
  /// throws [QazCancelled].
  Future<void> cancel() async {
    await QazVoice._instance._platform.cancel(id);
    _complete(const QazCancelled('download cancelled'));
  }

  /// Removes the model from the device (closing engines that use it).
  Future<void> delete() async {
    await QazVoice._instance._closeEnginesOf(id);
    await QazVoice._instance._platform.delete(id);
    _installedVersion = null;
    _installedBytes = 0;
    _complete(const QazCancelled('model deleted'));
    _set(const NotInstalled());
  }

  /// Throws [QazModelNotInstalled] unless installed.
  void ensureInstalled() {
    if (_installedVersion == null) throw QazModelNotInstalled(id);
  }

  void _complete([Object? error]) {
    final c = _pending;
    if (c == null || c.isCompleted) return;
    _pending = null;
    error == null ? c.complete() : c.completeError(error);
  }

  void _set(ModelState s) {
    if (_state.value != s) _state.value = s;
  }

  void _onWire(Map<String, Object?> m) {
    int i(String k) => (m[k] as num?)?.toInt() ?? 0;
    final total = m['total'] == null ? downloadSize : i('total');
    final progress = DownloadProgress(
        received: i('received'),
        total: total,
        bytesPerSecond: (m['speed'] as num?)?.toDouble() ?? 0);
    switch (m['status']) {
      case 'downloading':
        _set(Downloading(progress));
        _progress.add(progress);
      case 'waiting':
        final r =
            WaitReason.values.where((w) => w.name == m['reason']).firstOrNull ??
                WaitReason.queued;
        _set(Waiting(r, progress));
      case 'paused':
        _set(Paused(progress));
      case 'verifying':
        _set(const Verifying());
      case 'installed':
        _installedVersion = i('version');
        _installedBytes = i('installedBytes');
        _set(
            Installed(version: _installedVersion!, sizeBytes: _installedBytes));
        _complete();
      case 'failed':
        final err = errorFromWire(m['error']);
        _set(Failed(err, partialBytes: i('received')));
        _complete(err);
      case 'notInstalled':
        if (m['keepInstalled'] == true && _installedVersion != null) {
          // A cancelled update: the old version is still there.
          _set(Installed(
              version: _installedVersion!, sizeBytes: _installedBytes));
        } else {
          _installedVersion = null;
          _installedBytes = 0;
          _set(NotInstalled(partialBytes: i('received')));
        }
        _complete(const QazCancelled('download cancelled'));
    }
  }

  void _initFrom(Map<String, Object?>? m) {
    if (m == null) return;
    final iv = (m['installedVersion'] as num?)?.toInt();
    if (iv != null) {
      _installedVersion = iv;
      _installedBytes = (m['installedBytes'] as num?)?.toInt() ?? 0;
    }
    if (m['status'] == null && iv != null) {
      _set(Installed(version: iv, sizeBytes: _installedBytes));
    } else if (m['status'] == 'notInstalled' && iv != null) {
      _set(Installed(version: iv, sizeBytes: _installedBytes));
    } else {
      _onWire({...m});
    }
  }

  @override
  String toString() => 'QazModel($id v$version, ${_state.value})';
}

/// Entry point of the plugin: set-up, the model catalog, downloads, device
/// checks and logs.
///
/// ```dart
/// await QazVoice.init();
/// final voice = QazVoice.model('tts-kk');
/// if (!voice.isInstalled) await voice.download();
/// final tts = await QazTts.open();
/// await tts.speak('Сәлеметсіз бе!');
/// ```
abstract final class QazVoice {
  static final _QazVoiceImpl _instance = _QazVoiceImpl();

  /// Starts the plugin. Safe to call more than once; later calls only update
  /// [mirrors] and [notification].
  ///
  /// * [mirrors] — extra download locations tried after the catalog's own
  ///   (GitHub) ones, or before them with [preferMirrors].
  /// * [catalogUrl] — where to fetch the latest catalog (defaults to the
  ///   `catalog` release of eq-devs/qazvoice-models). The catalog built into the
  ///   plugin is used when it cannot be fetched. Pass `fetchCatalog: false` to
  ///   never go online for it.
  /// * [notification] — texts of the Android download notification.
  static Future<void> init({
    List<QazMirror> mirrors = const [],
    bool preferMirrors = false,
    Uri? catalogUrl,
    bool fetchCatalog = true,
    QazNotificationConfig notification = const QazNotificationConfig(),
  }) =>
      _instance.init(
          mirrors, preferMirrors, catalogUrl, fetchCatalog, notification);

  /// Switches the plugin to fake engines and a fake platform, for widget and
  /// unit tests (no native code, no network). Call before [init].
  static void useMock({
    MockQazPlatform? platform,
    TtsBackend Function()? tts,
    SttBackend Function()? stt,
    PunctBackend Function()? punct,
  }) =>
      _instance.useMock(platform, tts, stt, punct);

  /// Whether [useMock] is active.
  static bool get isMock => _instance._mock;

  /// All models in the catalog.
  static List<QazModel> get models {
    _instance._checkInit();
    return List.unmodifiable(_instance._models.values);
  }

  /// The model with [id] (`tts-kk`, `stt-kk`, `punct-kk`).
  static QazModel model(String id) {
    _instance._checkInit();
    final m = _instance._models[id];
    if (m == null) throw ArgumentError.value(id, 'id', 'unknown model');
    return m;
  }

  /// Checks CPU, memory and free storage for model [id].
  static Future<DeviceCheck> checkDevice(String id) =>
      _instance.checkDevice(id);

  /// Bytes used by installed models and partial downloads.
  static Future<int> get storageUsed => _instance._platform.storageUsed();

  /// License notices of all catalog models.
  static List<QazAttribution> get attributions => [
        for (final m in models)
          QazAttribution(
              modelId: m.id,
              name: m.name(),
              license: m.license,
              text: m.attribution)
      ];

  /// Plugin version.
  static String get version => qazvoicePluginVersion;

  /// Plugin log (downloads, model loads, errors). Also printed in debug builds.
  static Stream<QazLog> get onLog => _instance._log.stream;

  /// Asks for the notification permission (Android 13+) used by the download
  /// notification. `download()` works without it, just without a visible
  /// notification on those versions.
  static Future<bool> requestNotificationPermission() {
    _instance._checkInit();
    return _instance._platform.requestNotificationPermission();
  }

  /// Internal plumbing for the engine classes.
  static QazInternals get internals => _instance;
}

/// Internal interface used by QazTts / QazStt / QazText. Not for apps.
abstract class QazInternals {
  QazPlatform get platform;
  TtsBackend newTts();
  SttBackend newStt();
  PunctBackend newPunct();
  void register(QazEngineHolder e);
  void unregister(QazEngineHolder e);
  void log(QazLogLevel level, String message);
  QazModel model(String id);
  Stream<Map<String, Object?>> get events;
  bool get isMock;
  Future<void> ensureInit();
}

class _QazVoiceImpl implements QazInternals {
  QazPlatform _platform = MethodChannelQazPlatform();
  bool _mock = false;
  TtsBackend Function() _tts = NativeTtsBackend.new;
  SttBackend Function() _stt = NativeSttBackend.new;
  PunctBackend Function() _punct = NativePunctBackend.new;
  final _log = StreamController<QazLog>.broadcast();
  final Map<String, QazModel> _models = {};
  final Set<QazEngineHolder> _engines = {};
  List<QazMirror> _mirrors = const [];
  bool _preferMirrors = false;
  QazNotificationConfig _notification = const QazNotificationConfig();
  String _modelsDir = '';
  Future<void>? _initializing;
  bool _ready = false;
  StreamSubscription<Map<String, Object?>>? _sub;
  Stream<Map<String, Object?>>? _events;

  @override
  QazPlatform get platform => _platform;

  @override
  bool get isMock => _mock;

  @override
  TtsBackend newTts() => _tts();
  @override
  SttBackend newStt() => _stt();
  @override
  PunctBackend newPunct() => _punct();

  @override
  void register(QazEngineHolder e) => _engines.add(e);
  @override
  void unregister(QazEngineHolder e) => _engines.remove(e);

  @override
  QazModel model(String id) => QazVoice.model(id);

  @override
  Stream<Map<String, Object?>> get events => _events ??= _platform.events;

  @override
  void log(QazLogLevel level, String message) {
    final r = QazLog(level, message, DateTime.now());
    if (kDebugMode && level.index >= QazLogLevel.warning.index) debugPrint('$r');
    _log.add(r);
  }

  void _checkInit() {
    if (!_ready) {
      throw StateError('call `await QazVoice.init()` first');
    }
  }

  @override
  Future<void> ensureInit() async {
    if (_ready) return;
    await (_initializing ?? QazVoice.init());
  }

  void useMock(MockQazPlatform? platform, TtsBackend Function()? tts,
      SttBackend Function()? stt, PunctBackend Function()? punct) {
    _mock = true;
    _platform = platform ?? MockQazPlatform();
    _tts = tts ?? MockTtsBackend.new;
    _stt = stt ?? MockSttBackend.new;
    _punct = punct ?? MockPunctBackend.new;
    _ready = false;
    _initializing = null;
    _sub?.cancel();
    _sub = null;
    _events = null;
    _models.clear();
    _engines.clear();
  }

  Future<void> init(List<QazMirror> mirrors, bool preferMirrors,
      Uri? catalogUrl, bool fetchCatalog, QazNotificationConfig notification) {
    for (final m in mirrors) {
      if (!m.isValid) {
        throw ArgumentError.value(
            m.urlTemplate, 'mirrors', 'not an http(s) URL');
      }
    }
    _mirrors = List.unmodifiable(mirrors);
    _preferMirrors = preferMirrors;
    _notification = notification;
    return _initializing ??=
        _init(catalogUrl, fetchCatalog).catchError((Object e) {
      _initializing = null;
      throw e;
    });
  }

  Future<void> _init(Uri? catalogUrl, bool fetchCatalog) async {
    _sub ??= events.listen(_onEvent, onError: (Object e) {
      log(QazLogLevel.error, 'event channel: $e');
    });
    final r = await _platform.init({'notification': _notification.toMap()});
    _modelsDir = r['modelsDir'] as String;
    final states = ((r['states'] as Map?) ?? const {}).cast<String, Object?>();
    var catalog =
        Catalog.parse(defaultCatalogJson, pluginVersion: qazvoicePluginVersion);
    final cached = await _readCachedCatalog();
    if (cached != null && _newer(cached, catalog)) catalog = cached;
    _applyCatalog(catalog);
    for (final m in _models.values) {
      m._initFrom((states[m.id] as Map?)?.cast<String, Object?>());
    }
    _ready = true;
    log(QazLogLevel.info,
        'ready: ${_models.length} models in $_modelsDir${_mock ? ' (mock)' : ''}');
    if (fetchCatalog && !_mock) {
      unawaited(_fetchCatalog(catalogUrl ??
          Uri.parse(
              'https://github.com/eq-devs/qazvoice-models/releases/download/catalog/catalog.json')));
    }
  }

  bool _newer(Catalog a, Catalog b) => a.updated.compareTo(b.updated) >= 0;

  void _applyCatalog(Catalog c) {
    c.models.forEach((id, spec) {
      final m = _models[id];
      if (m == null) {
        _models[id] = QazModel._(spec, _modelsDir);
      } else if (spec.version >= m._spec.version) {
        m._spec = spec;
        m._catalogTick.value++;
      }
    });
  }

  File get _catalogCache => File('$_modelsDir/catalog.json');

  Future<Catalog?> _readCachedCatalog() async {
    try {
      final f = _catalogCache;
      if (!await f.exists()) return null;
      return Catalog.parse(await f.readAsString(),
          pluginVersion: qazvoicePluginVersion);
    } catch (_) {
      return null;
    }
  }

  Future<void> _fetchCatalog(Uri url) async {
    final client = HttpClient()
      ..connectionTimeout = const Duration(seconds: 10);
    try {
      final req = await client.getUrl(url);
      final res = await req.close().timeout(const Duration(seconds: 15));
      if (res.statusCode != 200) {
        throw HttpException('HTTP ${res.statusCode}', uri: url);
      }
      final body = await res
          .transform(utf8.decoder)
          .timeout(const Duration(seconds: 15))
          .fold(StringBuffer(), (b, s) => b..write(s));
      if (body.length > 1 << 20) {
        throw const FormatException('catalog too large');
      }
      final c =
          Catalog.parse(body.toString(), pluginVersion: qazvoicePluginVersion);
      _applyCatalog(c);
      await Directory(_modelsDir).create(recursive: true);
      final tmp = File('${_catalogCache.path}.tmp');
      await tmp.writeAsString(body.toString(), flush: true);
      await tmp.rename(_catalogCache.path);
      log(QazLogLevel.info, 'catalog updated (${c.updated})');
    } catch (e) {
      log(QazLogLevel.warning, 'catalog not fetched, using built-in: $e');
    } finally {
      client.close(force: true);
    }
  }

  void _onEvent(Map<String, Object?> e) {
    switch (e['type']) {
      case 'download':
        final m = _models[e['id']];
        if (m == null) return;
        final before = m._state.value;
        m._onWire(e);
        final after = m._state.value;
        if (before.runtimeType != after.runtimeType) {
          log(after is Failed ? QazLogLevel.error : QazLogLevel.info,
              '${m.id}: $after');
        }
      case 'trimMemory':
        final level = (e['level'] as num?)?.toInt() ?? 0;
        log(QazLogLevel.info,
            'memory pressure (level $level): releasing idle models');
        for (final eng in List.of(_engines)) {
          unawaited(eng.releaseIfIdle());
        }
      case 'log':
        log(
            QazLogLevel.values.where((l) => l.name == e['level']).firstOrNull ??
                QazLogLevel.info,
            '${e['message']}');
    }
  }

  Future<void> _closeEnginesOf(String id) async {
    for (final e in List.of(_engines)) {
      if (e.modelIds.contains(id)) await e.closeForModelChange();
    }
  }

  Future<void> _startDownload(QazModel m, bool wifiOnly) async {
    final spec = m._spec;
    final ordered = _preferMirrors
        ? [..._mirrors, ...spec.mirrors.map(QazMirror.new)]
        : [...spec.mirrors.map(QazMirror.new), ..._mirrors];
    if (ordered.isEmpty) throw const QazNetworkError('no mirrors configured');
    if (m.updateAvailable) await _closeEnginesOf(m.id);
    await _platform.download({
      'id': spec.id,
      'version': spec.version,
      'name': spec.name(_notification.language),
      'wifiOnly': wifiOnly,
      'files': [
        for (final f in spec.files)
          {
            ...f.toJson(),
            'urls': [
              for (final mirror in ordered)
                {
                  'url': mirror.resolve(spec, f.name).toString(),
                  'headers': mirror.headers
                }
            ],
          }
      ],
    });
  }

  Future<DeviceCheck> checkDevice(String id) async {
    final m = QazVoice.model(id);
    final d = await _platform.device();
    final abis = [for (final a in (d['abis'] as List? ?? const [])) '$a'];
    const shipped = {'arm64-v8a', 'armeabi-v7a', 'x86_64'};
    var lib = true;
    if (!_mock) {
      lib = QvLib.isAvailable;
    }
    final s = m._state.value;
    final have = switch (s) {
      Installed() when !m.updateAvailable => m.downloadSize,
      Downloading(:final progress) ||
      Paused(:final progress) ||
      Waiting(:final progress) =>
        progress.received,
      NotInstalled(:final partialBytes) ||
      Failed(:final partialBytes) =>
        partialBytes,
      _ => 0,
    };
    return DeviceCheck(
      modelId: id,
      abis: abis,
      supportedAbi: _mock || abis.any(shipped.contains),
      totalRamBytes: (d['totalRam'] as num?)?.toInt() ?? 0,
      availableRamBytes: (d['availRam'] as num?)?.toInt() ?? 0,
      freeBytes: (d['freeBytes'] as num?)?.toInt() ?? 0,
      requiredBytes: (m.downloadSize - have).clamp(0, 1 << 50),
      requiredRamBytes: m.spec.ramMb * 1024 * 1024,
      nativeLibrary: lib,
    );
  }
}
