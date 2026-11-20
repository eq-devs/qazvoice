import 'package:flutter/material.dart';
import 'package:qazvoice/qazvoice.dart';

import 'models_page.dart';
import 'stt_page.dart';
import 'text_page.dart';
import 'tts_page.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  runApp(const QazVoiceDemo());
}

class QazVoiceDemo extends StatelessWidget {
  const QazVoiceDemo({super.key});

  @override
  Widget build(BuildContext context) {
    const seed = Color(0xFF00A3C4); // Kazakh flag sky blue
    return MaterialApp(
      title: 'QazVoice',
      debugShowCheckedModeBanner: false,
      theme: ThemeData(colorSchemeSeed: seed, useMaterial3: true),
      darkTheme: ThemeData(
          colorSchemeSeed: seed,
          brightness: Brightness.dark,
          useMaterial3: true),
      home: const Home(),
    );
  }
}

class Home extends StatefulWidget {
  const Home({super.key});

  @override
  State<Home> createState() => _HomeState();
}

class _HomeState extends State<Home> {
  int _tab = 0;
  Object? _error;
  bool _ready = false;

  @override
  void initState() {
    super.initState();
    _init();
  }

  Future<void> _init() async {
    try {
      // A development mirror can be passed at build time, e.g.
      // --dart-define=QAZVOICE_MIRROR=http://10.0.2.2:8765/{id}/{file}
      const mirror = String.fromEnvironment('QAZVOICE_MIRROR');
      await QazVoice.init(
        notification: const QazNotificationConfig.kk(),
        mirrors: [if (mirror.isNotEmpty) const QazMirror(mirror)],
        preferMirrors: mirror.isNotEmpty,
      );
      // Shown on Android 13+: the download notification needs it.
      await QazVoice.requestNotificationPermission();
      if (mounted) setState(() => _ready = true);
    } catch (e) {
      if (mounted) setState(() => _error = e);
    }
  }

  @override
  Widget build(BuildContext context) {
    if (_error != null) {
      return Scaffold(body: Center(child: Text('Қате: $_error')));
    }
    if (!_ready) {
      return const Scaffold(body: Center(child: CircularProgressIndicator()));
    }
    const pages = [ModelsPage(), TtsPage(), SttPage(), TextPage()];
    const titles = ['Модельдер', 'Дауыстау', 'Тану', 'Мәтін'];
    return Scaffold(
      appBar: AppBar(title: Text(titles[_tab])),
      body: IndexedStack(index: _tab, children: pages),
      bottomNavigationBar: NavigationBar(
        selectedIndex: _tab,
        onDestinationSelected: (i) => setState(() => _tab = i),
        destinations: const [
          NavigationDestination(
              icon: Icon(Icons.download_for_offline_outlined),
              selectedIcon: Icon(Icons.download_for_offline),
              label: 'Модельдер'),
          NavigationDestination(
              icon: Icon(Icons.record_voice_over_outlined),
              selectedIcon: Icon(Icons.record_voice_over),
              label: 'Дауыстау'),
          NavigationDestination(
              icon: Icon(Icons.mic_none),
              selectedIcon: Icon(Icons.mic),
              label: 'Тану'),
          NavigationDestination(
              icon: Icon(Icons.text_fields_outlined),
              selectedIcon: Icon(Icons.text_fields),
              label: 'Мәтін'),
        ],
      ),
    );
  }
}
