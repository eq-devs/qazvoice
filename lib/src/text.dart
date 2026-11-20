import 'dart:async';

import 'package:flutter/foundation.dart';

import 'backends.dart';
import 'engine.dart';
import 'native/ffi.dart';
import 'voice.dart';

/// A sentence of a text with its UTF-16 span (`text.substring(start, end)`).
@immutable
class QazSentence {
  const QazSentence(this.text, this.start, this.end);
  final String text;
  final int start, end;
  @override
  bool operator ==(Object other) =>
      other is QazSentence &&
      other.text == text &&
      other.start == start &&
      other.end == end;
  @override
  int get hashCode => Object.hash(text, start, end);
  @override
  String toString() => 'QazSentence($start–$end "$text")';
}

/// Kazakh text utilities. All but [punctuate] need no model and are fast
/// enough to call on the UI isolate.
abstract final class QazText {
  static QvLib get _lib => QvLib.instance;

  /// Cyrillic → Latin (2021 Kazakh alphabet): "Қазақстан" → "Qazaqstan".
  static String toLatin(String text) => _lib.text(_lib.toLatinFn, text);

  /// Latin (2021 alphabet or plain ASCII) → Cyrillic; other text unchanged.
  static String toCyrillic(String text) => _lib.text(_lib.toCyrillicFn, text);

  /// Number words → digits: "жиырма бес адам" → "25 адам".
  static String numbersToDigits(String text) =>
      _lib.text(_lib.numbersToDigitsFn, text);

  /// Digits → number words: "25 адам" → "жиырма бес адам".
  static String spellNumbers(String text) =>
      _lib.text(_lib.spellNumbersFn, text);

  /// Splits [text] into sentences (abbreviations like "т.б." do not end one).
  static List<QazSentence> split(String text) => [
        for (final m in (_lib.textJson(_lib.splitFn, text) as List).cast<Map>())
          QazSentence('${m['text']}', (m['start'] as num).toInt(),
              (m['end'] as num).toInt())
      ];

  static _Punct? _punct;

  /// Restores punctuation and capitals with the `punct-kk` model (throws
  /// `QazModelNotInstalled` if it is not downloaded). Runs on a background
  /// isolate; the model stays loaded for a minute after the last call.
  static Future<String> punctuate(String text) async {
    final internals = QazVoice.internals;
    await internals.ensureInit();
    final model = internals.model(_Punct.id)..ensureInstalled();
    var p = _punct;
    if (p == null || p.isClosed) {
      p = _punct = _Punct(internals.newPunct(), model.directory);
      internals.register(p);
    }
    final engine = p;
    return engine.use(() => engine.backend.apply(text));
  }

  /// Frees the punctuation model now.
  static Future<void> releasePunctuation() async {
    final p = _punct;
    _punct = null;
    await p?.close();
  }
}

class _Punct extends ModelEngine {
  _Punct(this.backend, this.dir) : super(const Duration(minutes: 1));
  static const id = 'punct-kk';
  final PunctBackend backend;
  final String dir;
  @override
  Iterable<String> get modelIds => const [id];
  @override
  bool get isLoaded => backend.isLoaded;
  @override
  Future<void> doLoad() => backend.load(dir, 2);
  @override
  Future<void> doUnload() => backend.unload();
  @override
  Future<void> doClose() => backend.dispose();
}
