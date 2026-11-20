import 'dart:async';

import 'package:file_picker/file_picker.dart';
import 'package:flutter/material.dart';
import 'package:path_provider/path_provider.dart';
import 'package:qazvoice/qazvoice.dart';

import 'common.dart';

class SttPage extends StatelessWidget {
  const SttPage({super.key});

  @override
  Widget build(BuildContext context) =>
      const NeedsModel(modelIds: [QazStt.modelId], child: _Stt());
}

class _Stt extends StatefulWidget {
  const _Stt();

  @override
  State<_Stt> createState() => _SttState();
}

class _SttState extends State<_Stt> {
  QazStt? _stt;
  bool _punctuation = false;
  bool _highAccuracy = false;
  bool _numbers = true;
  bool _latin = false;
  bool _autoStop = true;
  final _hotwords = TextEditingController(text: 'Алматы, Астана');
  final List<SttResult> _finals = [];
  String _interim = '';
  SttSession? _session;
  final List<StreamSubscription<Object?>> _subs = [];
  bool _speech = false;
  double? _fileProgress;
  QazCancelToken? _fileToken;
  String? _status;
  bool _opening = false;

  @override
  void initState() {
    super.initState();
    _reopen();
  }

  @override
  void dispose() {
    for (final s in _subs) {
      s.cancel();
    }
    _stt?.close();
    _hotwords.dispose();
    super.dispose();
  }

  List<String> get _words => _hotwords.text
      .split(',')
      .map((s) => s.trim())
      .where((s) => s.isNotEmpty)
      .toList();

  /// Options are fixed per engine; reopen when they change.
  Future<void> _reopen() async {
    final old = _stt;
    setState(() {
      _stt = null;
      _opening = true;
    });
    await old?.close();
    try {
      final punct =
          _punctuation && QazVoice.model(QazStt.punctModelId).isInstalled;
      final giga = _highAccuracy &&
          QazVoice.model(QazStt.highAccuracyModelId).isInstalled;
      final stt = await QazStt.open(
        model: giga ? QazStt.highAccuracyModelId : QazStt.modelId,
        punctuation: punct,
        numbersToDigits: _numbers,
        latin: _latin,
        hotwords: _words,
      );
      final t = Stopwatch()..start();
      await stt.preload();
      if (!mounted) {
        await stt.close();
        return;
      }
      setState(() {
        _stt = stt;
        _status = 'Модель ${t.elapsedMilliseconds} мс ішінде жүктелді';
      });
    } catch (e) {
      if (mounted) showError(context, e);
    } finally {
      if (mounted) setState(() => _opening = false);
    }
  }

  void _attach(SttSession s) {
    for (final sub in _subs) {
      sub.cancel();
    }
    _subs.clear();
    _session = s;
    _subs.add(s.results.listen((r) {
      setState(() {
        if (r.isFinal) {
          _finals.add(r);
          _interim = '';
        } else {
          _interim = r.text;
        }
      });
    }, onError: (Object e) {
      if (mounted) showError(context, e);
    }));
    _subs.add(s.events
        .listen((e) => setState(() => _speech = e == SttEvent.speechStart)));
  }

  Future<void> _listen() async {
    final stt = _stt;
    if (stt == null) return;
    try {
      setState(() {
        _finals.clear();
        _interim = '';
      });
      final s = await stt.listen(
          autoStopAfter: _autoStop ? const Duration(seconds: 3) : null,
          maxDuration: const Duration(minutes: 5));
      _attach(s);
      setState(() {});
    } catch (e) {
      if (mounted) showError(context, e);
    }
  }

  Future<void> _transcribe(String path) async {
    final stt = _stt;
    if (stt == null) return;
    final token = QazCancelToken();
    setState(() {
      _finals.clear();
      _interim = '';
      _fileProgress = 0;
      _fileToken = token;
    });
    final t = Stopwatch()..start();
    try {
      final r = await stt.transcribeFile(path,
          cancelToken: token,
          onProgress: (p) => setState(() => _fileProgress = p),
          onResult: (r) => setState(() => _finals.add(r)));
      if (mounted) {
        setState(() => _status =
            '${r.length} бөлік · ${formatDuration(t.elapsed)}${r.isEmpty ? ' · сөйлеу табылмады' : ''}');
      }
    } catch (e) {
      if (mounted) showError(context, e);
    } finally {
      if (mounted) {
        setState(() {
          _fileProgress = null;
          _fileToken = null;
        });
      }
    }
  }

  Future<void> _pickFile() async {
    final files = await FilePicker.pickFiles(type: FileType.any);
    final path = files.isEmpty ? null : files.first.path;
    if (path != null) await _transcribe(path);
  }

  /// Makes a sample with the voice (if installed) and transcribes it.
  Future<void> _sample() async {
    if (!QazVoice.model(QazTts.modelId).isInstalled) {
      showError(context, const QazModelNotInstalled(QazTts.modelId));
      return;
    }
    final tts = await QazTts.open();
    try {
      final dir = await getTemporaryDirectory();
      final f = await tts.saveAudio('${dir.path}/sample.wav',
          'Алматы қаласында жиырма бес мың адам тұрады. Ертең күн жылы болады.',
          speaker: 3);
      await _transcribe(f.path);
    } finally {
      await tts.close();
    }
  }

  @override
  Widget build(BuildContext context) {
    final stt = _stt;
    final theme = Theme.of(context);
    final punctInstalled = QazVoice.model(QazStt.punctModelId).isInstalled;
    final listening = stt?.listening;
    return ListView(
      padding: const EdgeInsets.all(16),
      children: [
        Card(
          child: Column(children: [
            SwitchListTile(
              title: const Text('Тыныс белгілері'),
              subtitle: Text(punctInstalled
                  ? 'punct-kk моделі'
                  : '«punct-kk» моделін жүктеңіз'),
              value: _punctuation && punctInstalled,
              onChanged: punctInstalled
                  ? (v) {
                      setState(() => _punctuation = v);
                      _reopen();
                    }
                  : null,
            ),
            Builder(builder: (context) {
              final giga =
                  QazVoice.model(QazStt.highAccuracyModelId).isInstalled;
              return SwitchListTile(
                title: const Text('Жоғары дәлдік (GigaAM)'),
                subtitle: Text(giga
                    ? 'Қатесі ~24% аз, сәл баяу'
                    : '«stt-kk-giga» моделін жүктеңіз'),
                value: _highAccuracy && giga,
                onChanged: giga
                    ? (v) {
                        setState(() => _highAccuracy = v);
                        _reopen();
                      }
                    : null,
              );
            }),
            SwitchListTile(
              title: const Text('Сандарды цифрмен жазу'),
              value: _numbers,
              onChanged: (v) {
                setState(() => _numbers = v);
                _reopen();
              },
            ),
            SwitchListTile(
              title: const Text('Латын әліпбиі'),
              value: _latin,
              onChanged: (v) {
                setState(() => _latin = v);
                _reopen();
              },
            ),
            SwitchListTile(
              title: const Text('Үнсіздіктен кейін автоматты тоқтау'),
              value: _autoStop,
              onChanged: (v) => setState(() => _autoStop = v),
            ),
            Padding(
              padding: const EdgeInsets.fromLTRB(16, 0, 16, 16),
              child: TextField(
                controller: _hotwords,
                decoration: InputDecoration(
                  labelText: 'Арнайы сөздер (үтір арқылы)',
                  suffixIcon: IconButton(
                    tooltip: 'Қолдану',
                    icon: const Icon(Icons.check),
                    onPressed: stt == null
                        ? null
                        : () async {
                            await stt.setHotwords(_words);
                            if (context.mounted) {
                              ScaffoldMessenger.of(context).showSnackBar(
                                  const SnackBar(
                                      content: Text('Сөздер жаңартылды')));
                            }
                          },
                  ),
                ),
              ),
            ),
          ]),
        ),
        const SizedBox(height: 16),
        if (stt == null || _opening)
          const LinearProgressIndicator()
        else
          ValueListenableBuilder<bool>(
            valueListenable: listening!,
            builder: (context, on, _) => Column(children: [
              LevelMeter(
                  level: on ? _session?.level : null, bars: 32, height: 48),
              const SizedBox(height: 8),
              Text(on ? (_speech ? 'Сөйлеп жатырсыз…' : 'Тыңдап тұрмын…') : '',
                  style: theme.textTheme.bodySmall),
              const SizedBox(height: 8),
              Row(children: [
                Expanded(
                  child: on
                      ? FilledButton.tonalIcon(
                          key: const Key('stopListening'),
                          onPressed: stt.stopListening,
                          icon: const Icon(Icons.stop),
                          label: const Text('Тоқтату'))
                      : FilledButton.icon(
                          key: const Key('listen'),
                          onPressed: _fileProgress != null ? null : _listen,
                          icon: const Icon(Icons.mic),
                          label: const Text('Тыңдау')),
                ),
                const SizedBox(width: 8),
                IconButton.outlined(
                  tooltip: 'Файлды тану',
                  onPressed: on || _fileProgress != null ? null : _pickFile,
                  icon: const Icon(Icons.audio_file_outlined),
                ),
                IconButton.outlined(
                  key: const Key('sample'),
                  tooltip: 'Мысал аудио',
                  onPressed: on || _fileProgress != null ? null : _sample,
                  icon: const Icon(Icons.auto_awesome),
                ),
              ]),
            ]),
          ),
        if (_fileProgress != null) ...[
          const SizedBox(height: 12),
          Row(children: [
            Expanded(child: LinearProgressIndicator(value: _fileProgress)),
            TextButton(
                onPressed: () => _fileToken?.cancel(),
                child: const Text('Бас тарту')),
          ]),
        ],
        const SizedBox(height: 16),
        Card(
          child: Padding(
            padding: const EdgeInsets.all(16),
            child: ConstrainedBox(
              constraints: const BoxConstraints(minHeight: 120),
              child: SelectableText.rich(
                key: const Key('transcript'),
                TextSpan(style: theme.textTheme.bodyLarge, children: [
                  for (final r in _finals) TextSpan(text: '${r.text} '),
                  TextSpan(
                      text: _interim,
                      style: TextStyle(
                          color: theme.colorScheme.onSurfaceVariant,
                          fontStyle: FontStyle.italic)),
                  if (_finals.isEmpty && _interim.isEmpty)
                    TextSpan(
                        text: 'Мәтін осында шығады',
                        style: TextStyle(color: theme.colorScheme.outline)),
                ]),
              ),
            ),
          ),
        ),
        if (_status != null)
          Padding(
            padding: const EdgeInsets.only(top: 8),
            child: Text(_status!, style: theme.textTheme.bodySmall),
          ),
      ],
    );
  }
}
