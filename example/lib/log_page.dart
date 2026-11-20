import 'dart:async';

import 'package:flutter/material.dart';
import 'package:qazvoice/qazvoice.dart';

/// Keeps the plugin log (QazVoice.onLog) from app start, for the log screen.
class LogBuffer {
  LogBuffer._();
  static final instance = LogBuffer._();
  final List<QazLog> items = [];
  final _changes = StreamController<void>.broadcast();
  StreamSubscription<QazLog>? _sub;

  Stream<void> get changes => _changes.stream;

  void start() {
    _sub ??= QazVoice.onLog.listen((r) {
      items.add(r);
      if (items.length > 500) items.removeAt(0);
      _changes.add(null);
    });
  }
}

class LogPage extends StatelessWidget {
  const LogPage({super.key});

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Scaffold(
      appBar: AppBar(title: const Text('Плагин журналы')),
      body: StreamBuilder<void>(
        stream: LogBuffer.instance.changes,
        builder: (context, _) {
          final items = LogBuffer.instance.items.reversed.toList();
          if (items.isEmpty) {
            return const Center(child: Text('Әзірге жазба жоқ'));
          }
          return ListView.separated(
            itemCount: items.length,
            separatorBuilder: (_, __) => const Divider(height: 1),
            itemBuilder: (context, i) {
              final r = items[i];
              final color = switch (r.level) {
                QazLogLevel.error => theme.colorScheme.error,
                QazLogLevel.warning => Colors.orange,
                _ => theme.colorScheme.onSurfaceVariant,
              };
              final t = r.time;
              final hms = '${t.hour.toString().padLeft(2, '0')}:'
                  '${t.minute.toString().padLeft(2, '0')}:${t.second.toString().padLeft(2, '0')}';
              return ListTile(
                dense: true,
                leading: Icon(Icons.circle, size: 10, color: color),
                title: Text(r.message, style: theme.textTheme.bodySmall),
                trailing: Text(hms, style: theme.textTheme.labelSmall),
              );
            },
          );
        },
      ),
    );
  }
}
