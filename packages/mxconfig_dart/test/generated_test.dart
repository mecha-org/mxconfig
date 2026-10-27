// Tests the generated Dart client against a real namespace.
import 'dart:io';

import 'package:mxconfig_dart/mxconfig.dart';
import 'package:test/test.dart';

import '../example/generated/demo.dart';

/// The schema the client was generated from.
const schemaPath =
    '../../crates/mxconfig-example/schema/org.mechanix.demo.toml';

void main() {
  late Directory dir;

  setUp(() => dir = Directory.systemTemp.createTempSync('mxconfig-generated'));
  tearDown(() => dir.deleteSync(recursive: true));

  Future<Demo> open() => Settings.openIn(
        userDir: dir.path,
        schemaToml: File(schemaPath).readAsStringSync(),
      ).then(Demo.new);

  test('the generated open carries its schema when none is installed',
      () async {
    // The standard source finds its directories in the environment.
    final none = '${dir.path}/none';
    final run = await Process.run(Platform.resolvedExecutable, [
      'test/open_demo.dart'
    ], environment: {
      'MXCONFIG_USER_DIR': '${dir.path}/user',
      'MXCONFIG_SCHEMA_DIR': none,
      'MXCONFIG_VENDOR_DIR': none,
      'MXCONFIG_ADMIN_DIR': none,
      'MXCONFIG_MANIFEST_DIR': none,
    });
    expect(run.exitCode, 0, reason: '${run.stderr}');
    expect(run.stdout, '#7aa2f7\n');
    final written = File('${dir.path}/user/org.mechanix.demo/config.toml');
    expect(written.readAsStringSync(), contains('aux'));
  });

  test('the typed getters read the schema defaults', () async {
    final demo = await open();
    expect(demo.colors.accent, '#7aa2f7');
    expect(demo.colors.cornerRadius, 8);
    expect(demo.colors.mode, ColorsMode.dark);
    expect(demo.colors.scale, 1.0);
    expect(demo.verbose, isFalse);
    expect(demo.nickname, isNull, reason: 'no default, so unset');
    expect(demo.settings.describe.keys, hasLength(7));
  });

  test('the typed setters write, and enums carry the schema spelling',
      () async {
    final demo = await open();
    await demo.colors.setMode(ColorsMode.light);
    expect(demo.colors.mode, ColorsMode.light);
    expect(ColorsMode.light.value, 'light');
    expect(ColorsMode.parse('sepia'), isNull);

    await demo.setNickname('aux');
    expect(demo.nickname, 'aux');
    await demo.clearNickname();
    expect(demo.nickname, isNull);
  });

  test('a datetime key writes and reads its TOML text', () async {
    final demo = await open();
    expect(demo.lastSeen, isNull);
    await demo.setLastSeen('1979-05-27T07:32:00Z');
    expect(demo.lastSeen, '1979-05-27T07:32:00Z');
  });

  test('changes arrive typed, decoded to each key\'s own type', () async {
    final demo = await open();
    final seen = demo.changes().take(2).toList();
    await demo.colors.setMode(ColorsMode.light);
    await demo.setNickname('aux');
    final changes = await seen.timeout(const Duration(seconds: 5));
    expect(changes[0], isA<ColorsModeChange>());
    expect((changes[0] as ColorsModeChange).value, ColorsMode.light);
    expect((changes[1] as NicknameChange).value, 'aux');
    // A change logs as its key and value.
    expect('${changes[1]}', 'nickname = aux');
  });

  test('a v1 file migrates when a v2 client opens it', () async {
    final file = File('${dir.path}/org.mechanix.demo/config.toml')
      ..parent.createSync(recursive: true)
      ..writeAsStringSync('[colors]\nradius = 12\n');
    File('${file.path}.state').writeAsStringSync('version = 1\n');

    final demo = await open();
    expect(demo.settings.describe.version, 2);
    expect(demo.colors.cornerRadius, 12,
        reason: 'the renamed key kept its value');
    expect(demo.colors.mode, ColorsMode.dark,
        reason: 'the added key took the default');
  });
}
