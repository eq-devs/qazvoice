import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:qazvoice/qazvoice.dart';
import 'package:qazvoice_example/main.dart';

/// Lets real async work (mock timers, isolates) run, then rebuilds.
Future<void> settle(WidgetTester tester, [int ms = 300]) async {
  for (var i = 0; i < ms ~/ 50; i++) {
    await tester
        .runAsync(() => Future<void>.delayed(const Duration(milliseconds: 50)));
    await tester.pump(const Duration(milliseconds: 50));
  }
}

void main() {
  testWidgets('model manager downloads a model with the mock engine',
      (tester) async {
    QazVoice.useMock(platform: MockQazPlatform());
    await tester.pumpWidget(const QazVoiceDemo());
    await settle(tester);
    expect(find.text('Қазақша дауыс'), findsOneWidget);
    await tester.tap(find.text('Жүктеу').first);
    await settle(tester, 800);
    expect(find.textContaining('Орнатылған v1'), findsOneWidget);
    // The voice screen is now usable.
    await tester.tap(find.byIcon(Icons.record_voice_over_outlined));
    await settle(tester, 500);
    await tester.scrollUntilVisible(find.byKey(const Key('speak')), 200,
        scrollable: find.byType(Scrollable).last);
    expect(find.byKey(const Key('speak')), findsOneWidget);
    await tester.tap(find.byKey(const Key('speak')));
    await settle(tester, 300);
    // Text tools (mock mode has no native library: shows an error, no crash).
    await tester.tap(find.byIcon(Icons.text_fields_outlined));
    await settle(tester);
    expect(find.text('Латынға'), findsOneWidget);
  });
}
