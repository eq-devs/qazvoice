import 'package:flutter/material.dart';
import 'package:qazvoice/qazvoice.dart';

String formatBytes(num b) {
  if (b >= 1 << 30) return '${(b / (1 << 30)).toStringAsFixed(1)} ГБ';
  if (b >= 1 << 20) return '${(b / (1 << 20)).toStringAsFixed(1)} МБ';
  if (b >= 1 << 10) return '${(b / (1 << 10)).toStringAsFixed(0)} КБ';
  return '$b Б';
}

String formatDuration(Duration d) {
  final m = d.inMinutes;
  final s = d.inSeconds % 60;
  return m > 0 ? '$m мин $s с' : '$s с';
}

/// Kazakh text for every plugin error.
String describeError(Object e) => switch (e) {
      QazModelNotInstalled(:final modelId) => 'Алдымен «$modelId» моделін жүктеңіз',
      QazInsufficientStorage(:final neededBytes) =>
        'Жад жеткіліксіз: ${formatBytes(neededBytes)} бос орын керек',
      QazChecksumMismatch(:final fileName) => 'Файл бүлінген ($fileName), қайта жүктеңіз',
      QazNetworkError() => 'Желі қатесі: ${e.message}',
      QazUnsupportedDevice() => 'Бұл құрылғы қолдамайды: ${e.message}',
      QazPermissionDenied(:final permission) =>
        permission == 'microphone' ? 'Микрофонға рұқсат берілмеді' : 'Рұқсат берілмеді',
      QazCancelled() => 'Тоқтатылды',
      QazEngineError() => 'Қате: ${e.message}',
      _ => 'Қате: $e',
    };

void showError(BuildContext context, Object e) {
  ScaffoldMessenger.of(context)
    ..hideCurrentSnackBar()
    ..showSnackBar(SnackBar(content: Text(describeError(e))));
}

/// Shows [child] when [modelId] is installed, otherwise a hint to download it.
class NeedsModel extends StatelessWidget {
  const NeedsModel({super.key, required this.modelIds, required this.child});
  final List<String> modelIds;
  final Widget child;

  @override
  Widget build(BuildContext context) {
    final models = [for (final id in modelIds) QazVoice.model(id)];
    return ListenableBuilder(
      listenable: Listenable.merge([for (final m in models) m.state]),
      builder: (context, _) {
        final missing = models.where((m) => !m.isInstalled).toList();
        if (missing.isEmpty) return child;
        return Center(
          child: Padding(
            padding: const EdgeInsets.all(32),
            child: Column(mainAxisSize: MainAxisSize.min, children: [
              Icon(Icons.cloud_download_outlined,
                  size: 64, color: Theme.of(context).colorScheme.primary),
              const SizedBox(height: 16),
              Text(
                'Бұл бөлімге «${missing.map((m) => m.name('kk')).join('», «')}» моделі керек.\n'
                '«Модельдер» бетінде жүктеңіз.',
                textAlign: TextAlign.center,
                style: Theme.of(context).textTheme.bodyLarge,
              ),
            ]),
          ),
        );
      },
    );
  }
}

class SectionTitle extends StatelessWidget {
  const SectionTitle(this.text, {super.key});
  final String text;
  @override
  Widget build(BuildContext context) => Padding(
        padding: const EdgeInsets.fromLTRB(4, 16, 4, 8),
        child: Text(text,
            style: Theme.of(context)
                .textTheme
                .titleSmall
                ?.copyWith(color: Theme.of(context).colorScheme.primary)),
      );
}

/// A row of bars that follows a 0..1 level stream.
class LevelMeter extends StatefulWidget {
  const LevelMeter({super.key, required this.level, this.bars = 24, this.height = 40});
  final Stream<double>? level;
  final int bars;
  final double height;

  @override
  State<LevelMeter> createState() => _LevelMeterState();
}

class _LevelMeterState extends State<LevelMeter> {
  late List<double> _history = List.filled(widget.bars, 0);

  @override
  void didUpdateWidget(covariant LevelMeter oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.bars != widget.bars) _history = List.filled(widget.bars, 0);
  }

  @override
  Widget build(BuildContext context) {
    final color = Theme.of(context).colorScheme.primary;
    return StreamBuilder<double>(
      stream: widget.level,
      builder: (context, snap) {
        if (snap.hasData) {
          _history = [..._history.skip(1), snap.data!];
        }
        return SizedBox(
          height: widget.height,
          child: Row(
            mainAxisAlignment: MainAxisAlignment.center,
            crossAxisAlignment: CrossAxisAlignment.center,
            children: [
              for (final v in _history)
                AnimatedContainer(
                  duration: const Duration(milliseconds: 80),
                  margin: const EdgeInsets.symmetric(horizontal: 1.5),
                  width: 4,
                  height: 4 + (widget.height - 4) * v,
                  decoration: BoxDecoration(
                      color: color.withValues(alpha: 0.35 + 0.65 * v),
                      borderRadius: BorderRadius.circular(2)),
                ),
            ],
          ),
        );
      },
    );
  }
}
