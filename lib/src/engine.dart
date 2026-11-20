import 'dart:async';

import 'errors.dart';
import 'voice.dart';

/// Shared life cycle of the model-backed engines: lazy load, reload after a
/// release, idle timeout, release on memory pressure, close.
abstract class ModelEngine implements QazEngineHolder {
  ModelEngine(this.idleTimeout);

  /// Unload the model after this long without use (null = keep it loaded).
  final Duration? idleTimeout;

  int _busy = 0;
  Timer? _idle;
  bool _closed = false;
  Future<void>? _loading;

  bool get isClosed => _closed;

  /// Whether the model is in memory now.
  bool get isLoaded;

  /// Something (a live session, playback) needs the model to stay loaded.
  bool get pinned => false;

  Future<void> doLoad();
  Future<void> doUnload();
  Future<void> doClose();

  void checkOpen() {
    if (_closed) throw const QazEngineError('engine is closed', kind: 'config');
  }

  /// Loads the model now instead of on first use.
  Future<void> preload() => use(() async {});

  /// Runs [f] with the model loaded, keeping it from being released meanwhile.
  Future<T> use<T>(Future<T> Function() f) async {
    checkOpen();
    _busy++;
    _idle?.cancel();
    try {
      await _ensureLoaded();
      return await f();
    } finally {
      _busy--;
      _armIdle();
    }
  }

  Future<void> _ensureLoaded() async {
    if (isLoaded) return;
    final l = _loading ??= () async {
      final t = Stopwatch()..start();
      try {
        await doLoad();
        QazVoice.internals.log(QazLogLevel.info,
            '${modelIds.join('+')} loaded in ${t.elapsedMilliseconds} ms');
      } finally {
        _loading = null;
      }
    }();
    await l;
  }

  void _armIdle() {
    _idle?.cancel();
    final t = idleTimeout;
    if (t == null || _closed || _busy > 0) return;
    _idle = Timer(t, () => releaseIfIdle());
  }

  /// Keep the model loaded while [pinned] changes back to false.
  void touch() => _armIdle();

  @override
  Future<void> releaseIfIdle() async {
    if (_closed || _busy > 0 || pinned || !isLoaded || _loading != null) return;
    await doUnload();
    QazVoice.internals.log(QazLogLevel.info, '${modelIds.join('+')} released');
  }

  @override
  Future<void> closeForModelChange() => close();

  /// Frees the model and the background isolate. The object cannot be used after.
  Future<void> close() async {
    if (_closed) return;
    _closed = true;
    _idle?.cancel();
    QazVoice.internals.unregister(this);
    await doClose();
  }
}
