import 'dart:async';

import 'package:flutter/material.dart';
import 'package:path_provider/path_provider.dart';
import 'package:qazvoice/qazvoice.dart';
import 'package:share_plus/share_plus.dart';

import 'common.dart';

const _sample = 'Сәлеметсіз бе! Бұл қазақ тіліндегі дауыстау жүйесі. '
    'Ол интернетсіз, тікелей телефонда жұмыс істейді. '
    '2024 жылы 15 мамырда Алматыда 25 адам жиналды.';

class TtsPage extends StatelessWidget {
  const TtsPage({super.key});

  @override
  Widget build(BuildContext context) =>
      const NeedsModel(modelIds: [QazTts.modelId], child: _Tts());
}

class _Tts extends StatefulWidget {
  const _Tts();

  @override
  State<_Tts> createState() => _TtsState();
}

class _TtsState extends State<_Tts> {
  final _text = TextEditingController(text: _sample);
  QazTts? _tts;
  int _speaker = 0;
  double _speed = 1.0;
  bool _busy = false;
  bool _loading = true;
  String? _status;

  @override
  void initState() {
    super.initState();
    _open();
  }

  Future<void> _open() async {
    try {
      final tts = await QazTts.open();
      if (!mounted) {
        await tts.close();
        return;
      }
      setState(() => _tts = tts);
      // Load the model in the background so the first tap is fast.
      final t = Stopwatch()..start();
      await tts.preload();
      if (mounted) {
        setState(() {
          _loading = false;
          _status = 'Модель ${t.elapsedMilliseconds} мс ішінде жүктелді';
        });
      }
    } catch (e) {
      if (mounted) {
        setState(() => _loading = false);
        showError(context, e);
      }
    }
  }

  @override
  void dispose() {
    _tts?.close();
    _text.dispose();
    super.dispose();
  }

  /// "ISSAI_KazakhTTS_F1_Raya" → "Raya (әйел)", "ISSAI_KazakhTTS2_M2" → "Ер 2".
  static String _speakerLabel(String id) {
    final m = RegExp(r'_([FM])(\d+)(?:_(\w+))?$').firstMatch(id);
    if (m == null) return id;
    final gender = m.group(1) == 'F' ? 'Әйел' : 'Ер';
    final name = m.group(3);
    return name != null ? '$name (${gender.toLowerCase()})' : '$gender ${m.group(2)}';
  }

  Future<void> _speak() async {
    final tts = _tts;
    if (tts == null) return;
    try {
      final t = Stopwatch()..start();
      await tts.speak(_text.text, speaker: _speaker, speed: _speed);
      if (mounted) setState(() => _status = 'Оқылды (${formatDuration(t.elapsed)})');
    } catch (e) {
      if (mounted) showError(context, e);
    }
  }

  Future<void> _save({required bool share}) async {
    final tts = _tts;
    if (tts == null) return;
    setState(() => _busy = true);
    try {
      final dir = await getApplicationDocumentsDirectory();
      final path = '${dir.path}/qazvoice_${DateTime.now().millisecondsSinceEpoch}.m4a';
      final t = Stopwatch()..start();
      final f = await tts.saveAudio(path, _text.text,
          format: QazAudioFormat.m4a, speaker: _speaker, speed: _speed);
      final size = await f.length();
      if (!mounted) return;
      setState(() => _status = 'Сақталды: ${f.uri.pathSegments.last} · ${formatBytes(size)} · ${t.elapsedMilliseconds} мс');
      if (share) {
        await SharePlus.instance.share(ShareParams(files: [XFile(f.path, mimeType: 'audio/mp4')], text: _text.text));
      }
    } catch (e) {
      if (mounted) showError(context, e);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final tts = _tts;
    final theme = Theme.of(context);
    return ListView(
      padding: const EdgeInsets.all(16),
      children: [
        if (tts == null)
          const LinearProgressIndicator()
        else
          ValueListenableBuilder<bool>(
            valueListenable: tts.speaking,
            builder: (context, speaking, _) => speaking
                ? _Highlighted(tts: tts, text: _text.text)
                : TextField(
                    controller: _text,
                    minLines: 4,
                    maxLines: 10,
                    decoration: const InputDecoration(
                        border: OutlineInputBorder(), labelText: 'Мәтін'),
                  ),
          ),
        const SizedBox(height: 16),
        if (tts != null) ...[
          Text('Дауыс', style: theme.textTheme.titleSmall),
          const SizedBox(height: 8),
          Wrap(spacing: 8, runSpacing: 8, children: [
            for (var i = 0; i < tts.speakers.length; i++)
              ChoiceChip(
                label: Text(_speakerLabel(tts.speakers[i])),
                selected: _speaker == i,
                onSelected: (_) => setState(() => _speaker = i),
              ),
          ]),
          const SizedBox(height: 16),
          Text('Жылдамдық: ${_speed.toStringAsFixed(2)}×', style: theme.textTheme.titleSmall),
          Slider(
            value: _speed,
            min: 0.5,
            max: 2.0,
            divisions: 15,
            label: '${_speed.toStringAsFixed(2)}×',
            onChanged: (v) => setState(() => _speed = v),
          ),
          LevelMeter(level: tts.level, bars: 32, height: 48),
          const SizedBox(height: 16),
          ValueListenableBuilder<bool>(
            valueListenable: tts.speaking,
            builder: (context, speaking, _) => Row(children: [
              Expanded(
                child: speaking
                    ? FilledButton.tonalIcon(
                        key: const Key('stop'),
                        onPressed: tts.stop,
                        icon: const Icon(Icons.stop),
                        label: const Text('Тоқтату'))
                    : FilledButton.icon(
                        key: const Key('speak'),
                        onPressed: _loading ? null : _speak,
                        icon: const Icon(Icons.play_arrow),
                        label: const Text('Оқу')),
              ),
              const SizedBox(width: 8),
              IconButton.outlined(
                tooltip: 'm4a сақтау',
                onPressed: _busy || speaking ? null : () => _save(share: false),
                icon: const Icon(Icons.save_alt),
              ),
              IconButton.outlined(
                tooltip: 'Бөлісу',
                onPressed: _busy || speaking ? null : () => _save(share: true),
                icon: const Icon(Icons.share),
              ),
            ]),
          ),
          if (_busy) const Padding(padding: EdgeInsets.only(top: 12), child: LinearProgressIndicator()),
          if (_status != null)
            Padding(
              padding: const EdgeInsets.only(top: 12),
              child: Text(_status!, style: theme.textTheme.bodySmall),
            ),
          const SizedBox(height: 8),
          Text(
            'Модель: ${tts.isLoaded ? 'жадта' : 'жүктелмеген'} · ${tts.sampleRate} Гц',
            style: theme.textTheme.bodySmall,
          ),
        ],
      ],
    );
  }
}

/// The text with the sentence being read highlighted.
class _Highlighted extends StatelessWidget {
  const _Highlighted({required this.tts, required this.text});
  final QazTts tts;
  final String text;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final style = Theme.of(context).textTheme.bodyLarge!;
    return Container(
      padding: const EdgeInsets.all(12),
      decoration: BoxDecoration(
        border: Border.all(color: scheme.outline),
        borderRadius: BorderRadius.circular(4),
      ),
      child: ValueListenableBuilder<TtsChunk?>(
        valueListenable: tts.current,
        builder: (context, c, _) {
          if (c == null || c.end > text.length) return Text(text, style: style);
          return Text.rich(TextSpan(style: style, children: [
            TextSpan(text: text.substring(0, c.start)),
            TextSpan(
              text: text.substring(c.start, c.end),
              style: TextStyle(
                  backgroundColor: scheme.primaryContainer,
                  color: scheme.onPrimaryContainer,
                  fontWeight: FontWeight.w600),
            ),
            TextSpan(text: text.substring(c.end)),
          ]));
        },
      ),
    );
  }
}
