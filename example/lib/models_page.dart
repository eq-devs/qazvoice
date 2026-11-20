import 'package:flutter/material.dart';
import 'package:qazvoice/qazvoice.dart';

import 'common.dart';

/// Download manager: every model with its state, sizes, progress and actions;
/// device check, storage used and license notices.
class ModelsPage extends StatefulWidget {
  const ModelsPage({super.key});

  @override
  State<ModelsPage> createState() => _ModelsPageState();
}

class _ModelsPageState extends State<ModelsPage> {
  bool _wifiOnly = false;
  int? _used;

  @override
  void initState() {
    super.initState();
    _refreshUsed();
    for (final m in QazVoice.models) {
      m.state.addListener(_refreshUsed);
    }
  }

  @override
  void dispose() {
    for (final m in QazVoice.models) {
      m.state.removeListener(_refreshUsed);
    }
    super.dispose();
  }

  Future<void> _refreshUsed() async {
    final u = await QazVoice.storageUsed;
    if (mounted) setState(() => _used = u);
  }

  @override
  Widget build(BuildContext context) {
    return ListView(
      padding: const EdgeInsets.all(16),
      children: [
        Card(
          child: SwitchListTile(
            title: const Text('Тек Wi-Fi арқылы жүктеу'),
            subtitle: const Text('Мобильді интернетте жүктеу күтеді'),
            value: _wifiOnly,
            onChanged: (v) => setState(() => _wifiOnly = v),
          ),
        ),
        const SectionTitle('Модельдер'),
        for (final m in QazVoice.models)
          ModelCard(model: m, wifiOnly: _wifiOnly),
        const SectionTitle('Құрылғы'),
        Card(
          child: ListTile(
            leading: const Icon(Icons.sd_storage_outlined),
            title: const Text('Модельдер алатын орын'),
            trailing: Text(_used == null ? '…' : formatBytes(_used!)),
          ),
        ),
        const SectionTitle('Лицензиялар'),
        for (final a in QazVoice.attributions)
          Card(
            child: ListTile(
              title: Text('${a.modelId} · ${a.license}'),
              subtitle: Text(a.text),
            ),
          ),
        Padding(
          padding: const EdgeInsets.all(8),
          child: Text('qazvoice ${QazVoice.version}',
              textAlign: TextAlign.center,
              style: Theme.of(context).textTheme.bodySmall),
        ),
      ],
    );
  }
}

class ModelCard extends StatelessWidget {
  const ModelCard({super.key, required this.model, required this.wifiOnly});
  final QazModel model;
  final bool wifiOnly;

  Future<void> _download(BuildContext context) async {
    final check = await QazVoice.checkDevice(model.id);
    if (!context.mounted) return;
    if (!check.ok) {
      final go = await showDialog<bool>(
        context: context,
        builder: (c) => AlertDialog(
          title: const Text('Құрылғыны тексеру'),
          content: Text(check.problems.join('\n')),
          actions: [
            TextButton(
                onPressed: () => Navigator.pop(c, false),
                child: const Text('Бас тарту')),
            FilledButton(
                onPressed: () => Navigator.pop(c, true),
                child: const Text('Бәрібір жүктеу')),
          ],
        ),
      );
      if (go != true) return;
    }
    try {
      await model.download(wifiOnly: wifiOnly);
      if (context.mounted) {
        ScaffoldMessenger.of(context).showSnackBar(
            SnackBar(content: Text('«${model.name('kk')}» дайын')));
      }
    } on QazCancelled {
      // cancelled by the user
    } catch (e) {
      if (context.mounted) showError(context, e);
    }
  }

  Future<void> _checkDevice(BuildContext context) async {
    final c = await QazVoice.checkDevice(model.id);
    if (!context.mounted) return;
    await showDialog<void>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: Text(c.ok ? 'Құрылғы сәйкес келеді' : 'Ескерту'),
        content: Text([
          'Процессор: ${c.abis.join(', ')}',
          'Жедел жад: ${formatBytes(c.totalRamBytes)} (бос ${formatBytes(c.availableRamBytes)})',
          'Бос орын: ${formatBytes(c.freeBytes)}',
          'Жүктеу керек: ${formatBytes(c.requiredBytes)}',
          if (c.requiredRamBytes > 0)
            'Модельге жад: ~${formatBytes(c.requiredRamBytes)}',
          ...c.problems,
        ].join('\n')),
        actions: [
          TextButton(
              onPressed: () => Navigator.pop(ctx), child: const Text('Жабу'))
        ],
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Card(
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: ListenableBuilder(
          listenable: model.changes,
          builder: (context, _) {
            final s = model.state.value;
            return Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Row(children: [
                  Icon(switch (model.kind) {
                    QazModelKind.tts => Icons.record_voice_over,
                    QazModelKind.stt => Icons.hearing,
                    QazModelKind.punct => Icons.format_quote,
                  }),
                  const SizedBox(width: 12),
                  Expanded(
                    child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Text(model.name('kk'),
                              style: theme.textTheme.titleMedium),
                          Text(
                              '${model.id} · v${model.version} · жүктеу ${formatBytes(model.downloadSize)} · орын ${formatBytes(model.diskSize)}',
                              style: theme.textTheme.bodySmall),
                        ]),
                  ),
                  IconButton(
                    tooltip: 'Құрылғыны тексеру',
                    icon: const Icon(Icons.phone_android),
                    onPressed: () => _checkDevice(context),
                  ),
                ]),
                const SizedBox(height: 12),
                _status(context, s),
                const SizedBox(height: 8),
                Wrap(spacing: 8, runSpacing: 8, children: _actions(context, s)),
              ],
            );
          },
        ),
      ),
    );
  }

  Widget _status(BuildContext context, ModelState s) {
    Widget bar(DownloadProgress p, String text) => Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            LinearProgressIndicator(value: p.fraction),
            const SizedBox(height: 6),
            Text(text),
          ],
        );
    String eta(DownloadProgress p) =>
        p.eta == null ? '' : ' · ${formatDuration(p.eta!)} қалды';
    return switch (s) {
      NotInstalled(:final partialBytes) => Text(partialBytes > 0
          ? 'Жүктелмеген (${formatBytes(partialBytes)} сақталған)'
          : 'Жүктелмеген'),
      Downloading(:final progress) => bar(
          progress,
          '${formatBytes(progress.received)} / ${formatBytes(progress.total)} · '
          '${formatBytes(progress.bytesPerSecond)}/с${eta(progress)}'),
      Waiting(:final reason, :final progress) => bar(
          progress,
          switch (reason) {
            WaitReason.queued => 'Кезекте',
            WaitReason.network => 'Желі күтілуде…',
            WaitReason.wifi => 'Wi-Fi күтілуде…',
          }),
      Paused(:final progress) => bar(progress,
          'Кідіртілді · ${formatBytes(progress.received)} / ${formatBytes(progress.total)}'),
      Verifying() =>
        const Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
          LinearProgressIndicator(),
          SizedBox(height: 6),
          Text('Тексерілуде…'),
        ]),
      Installed(:final version, :final sizeBytes) => Text(
          model.updateAvailable
              ? 'Орнатылған v$version · жаңа нұсқа бар (v${model.version})'
              : 'Орнатылған v$version · ${formatBytes(sizeBytes)}',
          style: TextStyle(color: Theme.of(context).colorScheme.primary)),
      Failed(:final error) => Text(describeError(error),
          style: TextStyle(color: Theme.of(context).colorScheme.error)),
    };
  }

  List<Widget> _actions(BuildContext context, ModelState s) {
    final download = FilledButton.icon(
        onPressed: () => _download(context),
        icon: const Icon(Icons.download),
        label: const Text('Жүктеу'));
    final cancel = OutlinedButton.icon(
        onPressed: model.cancel,
        icon: const Icon(Icons.close),
        label: const Text('Бас тарту'));
    return switch (s) {
      NotInstalled() => [download],
      Failed() => [
          FilledButton.icon(
              onPressed: () => _download(context),
              icon: const Icon(Icons.refresh),
              label: const Text('Қайталау')),
          cancel,
        ],
      Downloading() || Waiting() => [
          FilledButton.tonalIcon(
              onPressed: model.pause,
              icon: const Icon(Icons.pause),
              label: const Text('Кідірту')),
          cancel,
        ],
      Paused() => [
          FilledButton.icon(
              onPressed: () => _download(context),
              icon: const Icon(Icons.play_arrow),
              label: const Text('Жалғастыру')),
          cancel,
        ],
      Verifying() => const [],
      Installed() => [
          if (model.updateAvailable)
            FilledButton.icon(
                onPressed: () => _download(context),
                icon: const Icon(Icons.system_update),
                label: const Text('Жаңарту')),
          OutlinedButton.icon(
            onPressed: () async {
              final ok = await showDialog<bool>(
                context: context,
                builder: (c) => AlertDialog(
                  title: const Text('Модельді өшіру керек пе?'),
                  content:
                      Text('${formatBytes(model.installedSize)} босатылады.'),
                  actions: [
                    TextButton(
                        onPressed: () => Navigator.pop(c, false),
                        child: const Text('Жоқ')),
                    FilledButton(
                        onPressed: () => Navigator.pop(c, true),
                        child: const Text('Өшіру')),
                  ],
                ),
              );
              if (ok == true) await model.delete();
            },
            icon: const Icon(Icons.delete_outline),
            label: const Text('Өшіру'),
          ),
        ],
    };
  }
}
