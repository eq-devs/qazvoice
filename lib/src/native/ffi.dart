import 'dart:convert';
import 'dart:ffi';
import 'dart:io';
import 'dart:typed_data';

import 'package:ffi/ffi.dart';

import '../errors.dart';

typedef QvStrFn = Pointer<Utf8> Function(Pointer<Utf8>);
typedef _VoidPtrN = Void Function(Pointer<Void>);
typedef QvVoidPtrFn = void Function(Pointer<Void>);
typedef QvPtrStrFn = Pointer<Utf8> Function(Pointer<Void>);
typedef QvPtrStrStrFn = Pointer<Utf8> Function(Pointer<Void>, Pointer<Utf8>);
typedef _NewPathN = Pointer<Void> Function(
    Pointer<Utf8>, Uint32, Pointer<Pointer<Utf8>>);
typedef QvNewPathFn = Pointer<Void> Function(
    Pointer<Utf8>, int, Pointer<Pointer<Utf8>>);
typedef QvNewCfgFn = Pointer<Void> Function(
    Pointer<Utf8>, Pointer<Pointer<Utf8>>);
typedef _SynthN = Pointer<Int16> Function(Pointer<Void>, Pointer<Utf8>, Uint32,
    Float, Pointer<Size>, Pointer<Pointer<Utf8>>);
typedef QvSynthFn = Pointer<Int16> Function(Pointer<Void>, Pointer<Utf8>, int,
    double, Pointer<Size>, Pointer<Pointer<Utf8>>);
typedef _FeedI16N = Pointer<Utf8> Function(
    Pointer<Void>, Pointer<Int16>, Size, Uint32, Uint16);
typedef QvFeedI16Fn = Pointer<Utf8> Function(
    Pointer<Void>, Pointer<Int16>, int, int, int);
typedef _FeedF32N = Pointer<Utf8> Function(
    Pointer<Void>, Pointer<Float>, Size, Uint32, Uint16);
typedef QvFeedF32Fn = Pointer<Utf8> Function(
    Pointer<Void>, Pointer<Float>, int, int, int);
typedef _PcmFreeN = Void Function(Pointer<Int16>, Size);
typedef _PcmFreeD = void Function(Pointer<Int16>, int);

/// The native library (`libqazvoice`), loaded once per isolate.
class QvLib {
  QvLib._(DynamicLibrary l)
      : _version = l.lookupFunction<Pointer<Utf8> Function(),
            Pointer<Utf8> Function()>('qv_version'),
        _stringFree = l.lookupFunction<Void Function(Pointer<Utf8>),
            void Function(Pointer<Utf8>)>('qv_string_free'),
        _pcmFree = l.lookupFunction<_PcmFreeN, _PcmFreeD>('qv_pcm_free'),
        toLatinFn = l.lookupFunction<QvStrFn, QvStrFn>('qv_to_latin'),
        toCyrillicFn = l.lookupFunction<QvStrFn, QvStrFn>('qv_to_cyrillic'),
        numbersToDigitsFn =
            l.lookupFunction<QvStrFn, QvStrFn>('qv_numbers_to_digits'),
        spellNumbersFn = l.lookupFunction<QvStrFn, QvStrFn>('qv_spell_numbers'),
        splitFn = l.lookupFunction<QvStrFn, QvStrFn>('qv_split'),
        ttsPlanFn = l.lookupFunction<QvStrFn, QvStrFn>('qv_tts_plan'),
        ttsNew = l.lookupFunction<_NewPathN, QvNewPathFn>('qv_tts_new'),
        ttsInfo = l.lookupFunction<QvPtrStrFn, QvPtrStrFn>('qv_tts_info'),
        ttsSynth = l.lookupFunction<_SynthN, QvSynthFn>('qv_tts_synthesize'),
        ttsFree = l.lookupFunction<_VoidPtrN, QvVoidPtrFn>('qv_tts_free'),
        sttNew = l.lookupFunction<QvNewCfgFn, QvNewCfgFn>('qv_stt_new'),
        sttFeedI16 =
            l.lookupFunction<_FeedI16N, QvFeedI16Fn>('qv_stt_feed_i16'),
        sttFeedF32 =
            l.lookupFunction<_FeedF32N, QvFeedF32Fn>('qv_stt_feed_f32'),
        sttPartial = l.lookupFunction<QvPtrStrFn, QvPtrStrFn>('qv_stt_partial'),
        sttFinish = l.lookupFunction<QvPtrStrFn, QvPtrStrFn>('qv_stt_finish'),
        sttReset = l.lookupFunction<_VoidPtrN, QvVoidPtrFn>('qv_stt_reset'),
        sttSetHotwords = l.lookupFunction<QvPtrStrStrFn, QvPtrStrStrFn>(
            'qv_stt_set_hotwords'),
        sttInfo = l.lookupFunction<QvPtrStrFn, QvPtrStrFn>('qv_stt_info'),
        sttFree = l.lookupFunction<_VoidPtrN, QvVoidPtrFn>('qv_stt_free'),
        punctNew = l.lookupFunction<_NewPathN, QvNewPathFn>('qv_punct_new'),
        punctApply =
            l.lookupFunction<QvPtrStrStrFn, QvPtrStrStrFn>('qv_punct_apply'),
        punctFree = l.lookupFunction<_VoidPtrN, QvVoidPtrFn>('qv_punct_free'),
        _verify = l.lookupFunction<
            Int32 Function(Pointer<Uint8>, Size, Pointer<Utf8>),
            int Function(
                Pointer<Uint8>, int, Pointer<Utf8>)>('qv_verify_catalog');

  final int Function(Pointer<Uint8>, int, Pointer<Utf8>) _verify;

  /// Whether [sigHex] is a valid signature of [data] by the catalog key.
  bool verifyCatalog(Uint8List data, String sigHex) => using((arena) {
        final p = arena<Uint8>(data.isEmpty ? 1 : data.length);
        p.asTypedList(data.length).setAll(0, data);
        return _verify(
                p, data.length, sigHex.trim().toNativeUtf8(allocator: arena)) ==
            1;
      });

  final Pointer<Utf8> Function() _version;
  final void Function(Pointer<Utf8>) _stringFree;
  final _PcmFreeD _pcmFree;
  final QvStrFn toLatinFn, toCyrillicFn, numbersToDigitsFn, spellNumbersFn;
  final QvStrFn splitFn, ttsPlanFn;
  final QvNewPathFn ttsNew;
  final QvPtrStrFn ttsInfo;
  final QvSynthFn ttsSynth;
  final QvVoidPtrFn ttsFree;
  final QvNewCfgFn sttNew;
  final QvFeedI16Fn sttFeedI16;
  final QvFeedF32Fn sttFeedF32;
  final QvPtrStrFn sttPartial, sttFinish, sttInfo;
  final QvVoidPtrFn sttReset, sttFree;
  final QvPtrStrStrFn sttSetHotwords;
  final QvNewPathFn punctNew;
  final QvPtrStrStrFn punctApply;
  final QvVoidPtrFn punctFree;

  /// Path of the library to load instead of the platform default
  /// (host tests: the macOS/Linux build of `rust/`). Pass it to worker isolates.
  static String? overridePath;

  static QvLib? _instance;
  static Object? _loadError;

  /// The library, loading it on first use. Throws [QazUnsupportedDevice].
  static QvLib get instance {
    final i = _instance;
    if (i != null) return i;
    if (_loadError != null) {
      throw QazUnsupportedDevice('native library unavailable: $_loadError');
    }
    try {
      return _instance = QvLib._(_open());
    } catch (e) {
      _loadError = e;
      throw QazUnsupportedDevice('native library unavailable: $e');
    }
  }

  /// Whether the library can be loaded here.
  static bool get isAvailable {
    try {
      instance;
      return true;
    } on QazUnsupportedDevice {
      return false;
    }
  }

  static DynamicLibrary _open() {
    final path = overridePath ?? Platform.environment['QAZVOICE_LIB'];
    if (path != null && path.isNotEmpty) return DynamicLibrary.open(path);
    if (Platform.isAndroid) return DynamicLibrary.open('libqazvoice.so');
    if (Platform.isIOS) return DynamicLibrary.process();
    if (Platform.isMacOS) return DynamicLibrary.open('libqazvoice.dylib');
    if (Platform.isWindows) return DynamicLibrary.open('qazvoice.dll');
    return DynamicLibrary.open('libqazvoice.so');
  }

  /// Native library version.
  String get version => _version().toDartString();

  /// Copies a returned string and frees the native one.
  String take(Pointer<Utf8> p) {
    if (p == nullptr) return '';
    try {
      return p.toDartString();
    } finally {
      _stringFree(p);
    }
  }

  /// Decodes returned JSON, throwing a [QazEngineError] for `{"error": …}`.
  Object? takeJson(Pointer<Utf8> p) {
    final v = jsonDecode(take(p));
    if (v is Map && v.containsKey('error')) {
      throw QazEngineError('${v['error']}', kind: '${v['kind'] ?? 'engine'}');
    }
    return v;
  }

  /// Copies PCM and frees the native buffer.
  Int16List takePcm(Pointer<Int16> p, int n) {
    if (p == nullptr || n == 0) return Int16List(0);
    try {
      return Int16List.fromList(p.asTypedList(n));
    } finally {
      _pcmFree(p, n);
    }
  }

  /// Runs a string → string function.
  String text(QvStrFn f, String input) =>
      using((arena) => take(f(input.toNativeUtf8(allocator: arena))));

  /// Runs a string → JSON function.
  Object? textJson(QvStrFn f, String input) =>
      using((arena) => takeJson(f(input.toNativeUtf8(allocator: arena))));

  /// Calls a constructor that reports failure through an `err_out` slot.
  Pointer<Void> create(Pointer<Void> Function(Pointer<Pointer<Utf8>> err) f) {
    return using((arena) {
      final err = arena<Pointer<Utf8>>()..value = nullptr;
      final h = f(err);
      if (h == nullptr) {
        final e = err.value;
        final msg = e == nullptr ? 'unknown error' : take(e);
        Object? v;
        try {
          v = jsonDecode(msg);
        } catch (_) {}
        if (v is Map) {
          throw QazEngineError('${v['error']}',
              kind: '${v['kind'] ?? 'model'}');
        }
        throw QazEngineError(msg, kind: 'model');
      }
      return h;
    });
  }
}
