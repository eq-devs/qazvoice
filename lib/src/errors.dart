/// Base class of every error thrown by qazvoice.
sealed class QazException implements Exception {
  const QazException(this.message);

  /// Human-readable description (English).
  final String message;

  @override
  String toString() => '$runtimeType: $message';
}

/// The model needed for this call is not downloaded yet.
/// Call `QazVoice.model(modelId).download()` first.
final class QazModelNotInstalled extends QazException {
  const QazModelNotInstalled(this.modelId)
      : super('model "$modelId" is not installed');

  /// Catalog id of the missing model, for example `tts-kk`.
  final String modelId;
}

/// Not enough free storage to finish a download.
final class QazInsufficientStorage extends QazException {
  const QazInsufficientStorage(this.neededBytes, {this.availableBytes})
      : super('not enough free storage: $neededBytes bytes needed');

  /// Bytes that must be free for the download to finish.
  final int neededBytes;

  /// Bytes free when the check ran, if known.
  final int? availableBytes;
}

/// A downloaded file did not match its SHA-256 on every mirror.
final class QazChecksumMismatch extends QazException {
  const QazChecksumMismatch(this.fileName)
      : super('checksum mismatch for $fileName');

  /// The file that failed verification.
  final String fileName;
}

/// The download failed on every mirror (no connection, server errors, 404…).
final class QazNetworkError extends QazException {
  const QazNetworkError(super.message, {this.statusCode});

  /// Last HTTP status, if a server answered.
  final int? statusCode;
}

/// This device cannot run the engine (unsupported CPU, native library missing,
/// too little memory to load the model…).
final class QazUnsupportedDevice extends QazException {
  const QazUnsupportedDevice(super.message);
}

/// The user denied a permission the call needs (microphone, notifications).
final class QazPermissionDenied extends QazException {
  const QazPermissionDenied(this.permission)
      : super('permission denied: $permission');

  /// `microphone` or `notifications`.
  final String permission;
}

/// The operation was cancelled (by `cancel()`, a cancel token or `stop()`).
final class QazCancelled extends QazException {
  const QazCancelled([super.message = 'cancelled']);
}

/// The engine failed: a model file could not be loaded, inference failed,
/// an audio file could not be decoded, or the plugin was used incorrectly.
final class QazEngineError extends QazException {
  const QazEngineError(super.message, {this.kind = 'engine'});

  /// `model`, `inference`, `audio`, `config`, `argument` or `engine`.
  final String kind;
}

/// Lets a caller stop a long operation such as `QazStt.transcribeFile`.
class QazCancelToken {
  bool _cancelled = false;
  final List<void Function()> _listeners = [];

  /// Whether [cancel] was called.
  bool get isCancelled => _cancelled;

  /// Requests cancellation. The operation then throws [QazCancelled].
  void cancel() {
    if (_cancelled) return;
    _cancelled = true;
    for (final l in List.of(_listeners)) {
      l();
    }
    _listeners.clear();
  }

  /// Internal: runs [listener] on cancel (at once if already cancelled).
  void addListener(void Function() listener) {
    if (_cancelled) {
      listener();
    } else {
      _listeners.add(listener);
    }
  }

  /// Internal.
  void removeListener(void Function() listener) => _listeners.remove(listener);
}

/// Maps an error crossing a platform/isolate boundary to a [QazException].
QazException errorFromWire(Object? e) {
  if (e is QazException) return e;
  if (e is Map) {
    final kind = '${e['kind'] ?? 'engine'}';
    final msg = '${e['message'] ?? e['error'] ?? 'unknown error'}';
    switch (kind) {
      case 'notInstalled':
        return QazModelNotInstalled('${e['model'] ?? ''}');
      case 'storage':
        return QazInsufficientStorage((e['needed'] as num?)?.toInt() ?? 0,
            availableBytes: (e['available'] as num?)?.toInt());
      case 'checksum':
        return QazChecksumMismatch('${e['file'] ?? ''}');
      case 'network':
        return QazNetworkError(msg, statusCode: (e['status'] as num?)?.toInt());
      case 'unsupported':
        return QazUnsupportedDevice(msg);
      case 'permission':
        return QazPermissionDenied('${e['permission'] ?? msg}');
      case 'cancelled':
        return QazCancelled(msg);
      default:
        return QazEngineError(msg, kind: kind);
    }
  }
  return QazEngineError('$e');
}

/// The inverse of [errorFromWire], for sending errors between isolates.
Map<String, Object?> errorToWire(Object e) => switch (e) {
      QazModelNotInstalled(:final modelId) => {
          'kind': 'notInstalled',
          'model': modelId,
          'message': e.message
        },
      QazInsufficientStorage(:final neededBytes, :final availableBytes) => {
          'kind': 'storage',
          'needed': neededBytes,
          'available': availableBytes,
          'message': e.message
        },
      QazChecksumMismatch(:final fileName) => {
          'kind': 'checksum',
          'file': fileName,
          'message': e.message
        },
      QazNetworkError(:final statusCode) => {
          'kind': 'network',
          'status': statusCode,
          'message': e.message
        },
      QazUnsupportedDevice() => {'kind': 'unsupported', 'message': e.message},
      QazPermissionDenied(:final permission) => {
          'kind': 'permission',
          'permission': permission,
          'message': e.message
        },
      QazCancelled() => {'kind': 'cancelled', 'message': e.message},
      QazEngineError(:final kind) => {'kind': kind, 'message': e.message},
      _ => {'kind': 'engine', 'message': '$e'},
    };
