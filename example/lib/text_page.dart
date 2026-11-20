import 'package:flutter/material.dart';
import 'package:qazvoice/qazvoice.dart';

import 'common.dart';

/// QazText: transliteration, numbers, sentence split, punctuation.
class TextPage extends StatefulWidget {
  const TextPage({super.key});

  @override
  State<TextPage> createState() => _TextPageState();
}

class _TextPageState extends State<TextPage> {
  final _input = TextEditingController(
      text:
          'бүгін алматыда жиырма бес градус жылы болады ертең жаңбыр жауады 2025 жылы т.б.');
  String _output = '';
  String _label = '';
  bool _busy = false;

  @override
  void dispose() {
    _input.dispose();
    super.dispose();
  }

  void _run(String label, String Function(String) f) {
    try {
      setState(() {
        _label = label;
        _output = f(_input.text);
      });
    } catch (e) {
      showError(context, e);
    }
  }

  Future<void> _punctuate() async {
    setState(() => _busy = true);
    try {
      final t = Stopwatch()..start();
      final r = await QazText.punctuate(_input.text);
      if (mounted) {
        setState(() {
          _label = 'Тыныс белгілері (${t.elapsedMilliseconds} мс)';
          _output = r;
        });
      }
    } catch (e) {
      if (mounted) showError(context, e);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return ListView(
      padding: const EdgeInsets.all(16),
      children: [
        TextField(
          controller: _input,
          minLines: 3,
          maxLines: 8,
          decoration: const InputDecoration(
              border: OutlineInputBorder(), labelText: 'Мәтін'),
        ),
        const SizedBox(height: 12),
        Wrap(spacing: 8, runSpacing: 8, children: [
          ActionChip(
              avatar: const Icon(Icons.translate, size: 18),
              label: const Text('Латынға'),
              onPressed: () => _run('Латын', QazText.toLatin)),
          ActionChip(
              avatar: const Icon(Icons.translate, size: 18),
              label: const Text('Кириллицаға'),
              onPressed: () => _run('Кирилл', QazText.toCyrillic)),
          ActionChip(
              avatar: const Icon(Icons.pin_outlined, size: 18),
              label: const Text('Сөз → цифр'),
              onPressed: () => _run('Цифрмен', QazText.numbersToDigits)),
          ActionChip(
              avatar: const Icon(Icons.abc, size: 18),
              label: const Text('Цифр → сөз'),
              onPressed: () => _run('Сөзбен', QazText.spellNumbers)),
          ActionChip(
              avatar: const Icon(Icons.segment, size: 18),
              label: const Text('Сөйлемдерге бөлу'),
              onPressed: () => _run(
                  'Сөйлемдер',
                  (t) => QazText.split(t)
                      .map((s) => '• ${s.text}  [${s.start}–${s.end}]')
                      .join('\n'))),
          ListenableBuilder(
            listenable: QazVoice.model('punct-kk').state,
            builder: (context, _) => ActionChip(
              avatar: const Icon(Icons.format_quote, size: 18),
              label: const Text('Тыныс белгілері'),
              onPressed: _busy || !QazVoice.model('punct-kk').isInstalled
                  ? null
                  : _punctuate,
            ),
          ),
        ]),
        if (_busy)
          const Padding(
              padding: EdgeInsets.only(top: 12),
              child: LinearProgressIndicator()),
        const SizedBox(height: 16),
        if (_output.isNotEmpty)
          Card(
            child: Padding(
              padding: const EdgeInsets.all(16),
              child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(_label,
                        style: theme.textTheme.titleSmall
                            ?.copyWith(color: theme.colorScheme.primary)),
                    const SizedBox(height: 8),
                    SelectableText(_output,
                        key: const Key('textOutput'),
                        style: theme.textTheme.bodyLarge),
                  ]),
            ),
          ),
      ],
    );
  }
}
