import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:qazvoice/qazvoice.dart';
import 'package:qazvoice/src/catalog.dart';
import 'package:qazvoice/src/default_catalog.dart';

void main() {
  test('built-in catalog parses and has the three models', () {
    final c = Catalog.parse(defaultCatalogJson, pluginVersion: '0.0.4');
    expect(c.models.keys,
        containsAll(['tts-kk', 'stt-kk', 'punct-kk', 'stt-kk-giga']));
    final tts = c.models['tts-kk']!;
    expect(tts.kind, QazModelKind.tts);
    expect(tts.files.map((f) => f.name), ['voice.onnx', 'voice.onnx.json']);
    expect(tts.size, 127864258 + 5990);
    // The voice is downloaded as zstd (smaller), the small JSON as is.
    expect(tts.files.first.zstSize, isNotNull);
    expect(tts.files.last.zstSize, isNull);
    expect(tts.downloadSize, lessThan(tts.size));
    expect(tts.name('kk'), 'Қазақша дауыс');
    expect(tts.attribution, contains('ISSAI'));
    expect(c.models['stt-kk']!.files.length, 5);
    expect(c.models['stt-kk-giga']!.license, 'MIT');
    // Older plugins never see the optional model.
    expect(
        Catalog.parse(defaultCatalogJson, pluginVersion: '0.0.3')
            .models
            .containsKey('stt-kk-giga'),
        isFalse);
  });

  test('mirror templates resolve and encode', () {
    final spec = Catalog.parse(defaultCatalogJson, pluginVersion: '0.0.1')
        .models['tts-kk']!;
    final m = QazMirror(spec.mirrors.first);
    expect(m.resolve(spec, 'voice.onnx').toString(),
        'https://github.com/eq-devs/qazvoice-models/releases/download/tts-kk-v1/voice.onnx');
    final own = const QazMirror('https://x.example/m/{id}/{version}/{file}');
    expect(own.resolve(spec, 'a b').toString(),
        'https://x.example/m/tts-kk/1/a%20b');
    expect(const QazMirror('ftp://x/{file}').isValid, isFalse);
    expect(const QazMirror('not a url').isValid, isFalse);
  });

  Map<String, Object?> base() =>
      jsonDecode(defaultCatalogJson) as Map<String, Object?>;

  test('unsafe or malformed entries are dropped', () {
    final j = base();
    final models = j['models'] as Map;
    (models['tts-kk'] as Map)['files'] = [
      {'name': '../evil', 'size': 1, 'sha256': 'a' * 64}
    ];
    (models['stt-kk'] as Map)['files'] = [
      {'name': 'ok.onnx', 'size': 1, 'sha256': 'XYZ'}
    ];
    models['Bad Id'] = models['punct-kk'];
    final c = Catalog.parse(jsonEncode(j), pluginVersion: '0.0.1');
    expect(c.models.keys, ['punct-kk']);
  });

  test('models needing a newer plugin are skipped', () {
    final j = base();
    ((j['models'] as Map)['tts-kk'] as Map)['minPlugin'] = '0.1.0';
    final c = Catalog.parse(jsonEncode(j), pluginVersion: '0.0.1');
    expect(c.models.containsKey('tts-kk'), isFalse);
    expect(
        Catalog.parse(jsonEncode(j), pluginVersion: '0.1.0')
            .models
            .containsKey('tts-kk'),
        isTrue);
  });

  test('wrong schema is rejected', () {
    expect(
        () => Catalog.parse('{"schema":2,"models":{}}', pluginVersion: '0.0.1'),
        throwsFormatException);
    expect(() => Catalog.parse('[]', pluginVersion: '0.0.1'),
        throwsFormatException);
  });

  test('version compare', () {
    expect(compareVersions('0.0.10', '0.0.9'), 1);
    expect(compareVersions('0.0.1', '0.0.1'), 0);
    expect(compareVersions('0.1.0-dev', '0.1.0'), 0);
    expect(compareVersions('1.0', '0.9.9'), 1);
  });

  test('safe file names', () {
    for (final n in ['voice.onnx', 'tokens.txt', 'a-b_c.json']) {
      expect(isSafeFileName(n), isTrue, reason: n);
    }
    for (final n in ['../x', '.hidden', 'a/b', 'installed.json', '', 'a..b']) {
      expect(isSafeFileName(n), isFalse, reason: n);
    }
  });

  test('errors survive the wire', () {
    for (final e in <QazException>[
      const QazModelNotInstalled('tts-kk'),
      const QazInsufficientStorage(10, availableBytes: 5),
      const QazChecksumMismatch('voice.onnx'),
      const QazNetworkError('down', statusCode: 503),
      const QazUnsupportedDevice('x86'),
      const QazPermissionDenied('microphone'),
      const QazCancelled(),
      const QazEngineError('boom', kind: 'inference'),
    ]) {
      final back = errorFromWire(errorToWire(e));
      expect(back.runtimeType, e.runtimeType);
      expect(back.message, e.message);
    }
    final s = errorFromWire({'kind': 'storage', 'needed': 7, 'message': 'x'})
        as QazInsufficientStorage;
    expect(s.neededBytes, 7);
  });
}
