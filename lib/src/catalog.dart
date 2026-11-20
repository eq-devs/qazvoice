import 'dart:convert';

/// What a model is for.
enum QazModelKind {
  /// Text-to-speech voice (used by `QazTts`).
  tts,

  /// Speech recognition (used by `QazStt`).
  stt,

  /// Punctuation and true-casing (used by `QazStt(punctuation: true)` and
  /// `QazText.punctuate`).
  punct,
}

/// A place model files can be downloaded from.
///
/// [urlTemplate] may contain `{id}`, `{version}`, `{release}` and `{file}`:
///
/// ```dart
/// QazMirror('https://models.example.com/qazvoice/{id}/{version}/{file}',
///     headers: {'Authorization': 'Bearer $key'})
/// ```
///
/// [headers] are sent only to this mirror's host; they are dropped if the
/// server redirects to another host.
class QazMirror {
  const QazMirror(this.urlTemplate, {this.headers = const {}});

  /// URL with placeholders.
  final String urlTemplate;

  /// Extra request headers (for example an API key).
  final Map<String, String> headers;

  /// The URL of [file] of [model].
  Uri resolve(ModelSpec model, String file) {
    final s = urlTemplate
        .replaceAll('{id}', Uri.encodeComponent(model.id))
        .replaceAll('{version}', '${model.version}')
        .replaceAll('{release}', Uri.encodeComponent(model.release))
        .replaceAll('{file}', Uri.encodeComponent(file));
    return Uri.parse(s);
  }

  /// Whether the template is a usable http(s) URL.
  bool get isValid {
    final u = Uri.tryParse(urlTemplate.replaceAll(RegExp(r'\{[a-z]+\}'), 'x'));
    return u != null &&
        (u.scheme == 'https' || u.scheme == 'http') &&
        u.host.isNotEmpty;
  }
}

/// One file of a model.
class ModelFile {
  const ModelFile(
      {required this.name, required this.size, required this.sha256});

  /// File name inside the model directory.
  final String name;

  /// Size in bytes.
  final int size;

  /// Lowercase hex SHA-256 of the file.
  final String sha256;

  Map<String, Object> toJson() =>
      {'name': name, 'size': size, 'sha256': sha256};
}

/// A model as described by the catalog.
class ModelSpec {
  const ModelSpec({
    required this.id,
    required this.version,
    required this.kind,
    required this.release,
    required this.names,
    required this.license,
    required this.attribution,
    required this.minPlugin,
    required this.files,
    required this.mirrors,
    this.ramMb = 0,
  });

  final String id;
  final int version;
  final QazModelKind kind;
  final String release;

  /// Display names by language code (`en`, `kk`, …).
  final Map<String, String> names;

  /// SPDX license expression of the model files.
  final String license;

  /// Attribution text to show in the app.
  final String attribution;

  /// Oldest plugin version that can use this model version.
  final String minPlugin;
  final List<ModelFile> files;

  /// Mirror URL templates from the catalog.
  final List<String> mirrors;

  /// Approximate memory needed to run the model, in MB (0 = unknown).
  final int ramMb;

  /// Sum of all file sizes.
  int get size => files.fold(0, (a, f) => a + f.size);

  /// The display name in [lang], falling back to English, then the id.
  String name([String lang = 'en']) => names[lang] ?? names['en'] ?? id;
}

final _idRe = RegExp(r'^[a-z0-9][a-z0-9-]{0,40}$');
final _fileRe = RegExp(r'^[A-Za-z0-9_-][A-Za-z0-9._-]{0,100}$');
final _shaRe = RegExp(r'^[0-9a-f]{64}$');

/// Whether [name] is safe to use as a file name inside a model directory.
bool isSafeFileName(String name) =>
    _fileRe.hasMatch(name) && !name.contains('..') && name != 'installed.json';

/// Parsed `catalog.json`.
class Catalog {
  const Catalog(this.models, {this.updated = ''});

  final Map<String, ModelSpec> models;
  final String updated;

  /// Parses and validates a catalog. Models that are malformed, unsafe or need
  /// a newer plugin than [pluginVersion] are skipped.
  static Catalog parse(String source, {required String pluginVersion}) {
    final root = jsonDecode(source);
    if (root is! Map || root['schema'] != 1 || root['models'] is! Map) {
      throw const FormatException('not a qazvoice catalog (schema 1)');
    }
    final models = <String, ModelSpec>{};
    (root['models'] as Map).forEach((key, value) {
      final spec = _model('$key', value);
      if (spec != null && compareVersions(spec.minPlugin, pluginVersion) <= 0) {
        models[spec.id] = spec;
      }
    });
    return Catalog(models, updated: '${root['updated'] ?? ''}');
  }

  static ModelSpec? _model(String id, Object? v) {
    if (!_idRe.hasMatch(id) || v is! Map) return null;
    final kind =
        QazModelKind.values.where((k) => k.name == v['kind']).firstOrNull;
    final version = v['version'];
    final files = v['files'];
    if (kind == null || version is! int || version < 1 || files is! List) {
      return null;
    }
    final parsed = <ModelFile>[];
    for (final f in files) {
      if (f is! Map) return null;
      final name = f['name'], size = f['size'], sha = f['sha256'];
      if (name is! String ||
          !isSafeFileName(name) ||
          size is! int ||
          size < 0 ||
          sha is! String ||
          !_shaRe.hasMatch(sha)) {
        return null;
      }
      parsed.add(ModelFile(name: name, size: size, sha256: sha));
    }
    if (parsed.isEmpty) return null;
    final names = <String, String>{};
    if (v['name'] is Map) {
      (v['name'] as Map).forEach((k, n) => names['$k'] = '$n');
    }
    return ModelSpec(
      id: id,
      version: version,
      kind: kind,
      release: '${v['release'] ?? '$id-v$version'}',
      names: names,
      license: '${v['license'] ?? ''}',
      attribution: '${v['attribution'] ?? ''}',
      minPlugin: '${v['minPlugin'] ?? '0.0.0'}',
      files: List.unmodifiable(parsed),
      mirrors: [
        for (final m in (v['mirrors'] as List? ?? const []))
          if (m is String && QazMirror(m).isValid) m
      ],
      ramMb: (v['ramMb'] as num?)?.toInt() ?? 0,
    );
  }
}

/// Compares dotted versions (`0.0.10` > `0.0.9`). Non-numeric parts count as 0.
int compareVersions(String a, String b) {
  List<int> p(String s) => s
      .split('+')
      .first
      .split('-')
      .first
      .split('.')
      .map((x) => int.tryParse(x) ?? 0)
      .toList();
  final x = p(a), y = p(b);
  for (var i = 0; i < 3; i++) {
    final d = (i < x.length ? x[i] : 0) - (i < y.length ? y[i] : 0);
    if (d != 0) return d.sign;
  }
  return 0;
}
