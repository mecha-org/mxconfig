import 'dart:async';
import 'dart:io';

import 'package:mxconfig_dart/mxconfig.dart';
import 'package:test/test.dart';

const schema = '''
namespace = "org.mechanix.demo"
version = 1

[groups.colors]
label = "Colors"
order = 10

[keys.colors.accent]
type = "string"
default = "#7aa2f7"
label = "Accent"
description = "The accent color."

[keys.colors.corner_radius]
type = "integer"
default = 8
label = "Corner radius"
description = "Corner radius in pixels."
min = 0
max = 64

[keys.colors.mode]
type = "enum"
values = ["light", "dark"]
default = "dark"
label = "Mode"
description = "Theme mode."

[keys.verbose]
type = "bool"
default = false
label = "Verbose"
description = "Verbose logging."
env = "MXCONFIG_DART_TEST_VERBOSE"

[keys.nickname]
type = "string"
label = "Nickname"
description = "Shown in the title bar; unset until the user picks one."
''';

const manifest = '''
client = "org.mechanix.demo"

[[settings]]
namespace = "org.mechanix.demo"
access = "read"
''';

const otherManifest = '''
client = "org.other.thing"

[[settings]]
namespace = "org.other.thing"
access = "read-write"
''';

void main() {
  late Directory dir;

  setUp(() => dir = Directory.systemTemp.createTempSync('mxconfig-dart'));
  tearDown(() => dir.deleteSync(recursive: true));

  Future<Settings> open() =>
      Settings.openIn(userDir: dir.path, schemaToml: schema);

  test('reads the schema defaults', () async {
    final settings = await open();

    expect(settings.namespace, 'org.mechanix.demo');
    expect(settings.revision, 0);

    expect(settings.getString('colors.accent'), '#7aa2f7');
    expect(settings.getInt('colors.corner_radius'), 8);
    expect(settings.getBool('verbose'), false);
    expect(settings.getString('nickname'), isNull);
    expect(settings.diagnostics, isEmpty);

    settings.dispose();
  });

  test('writes, validates and reports changes', () async {
    final settings = await open();

    // The app's own write arrives on watch() too.
    final changed = settings.watch().first;
    await settings.setString('colors.accent', '#ff0000');
    final change = await changed;
    expect(change.key, 'colors.accent');
    expect(change.value, '"#ff0000"');
    expect(change.isUnset, isFalse);
    expect(settings.getString('colors.accent'), '#ff0000');
    expect(settings.revision, 1);

    await settings.setInt('colors.corner_radius', 20);
    expect(settings.getInt('colors.corner_radius'), 20);

    await expectLater(
        settings.setInt('colors.corner_radius', 99), throwsA(anything));
    await expectLater(settings.setInt('colors.accent', 5), throwsA(anything));
    await expectLater(settings.setBool('nope.nope', true), throwsA(anything));

    await settings.setString('nickname', 'aux');
    final removed = settings.watch().first;
    await settings.remove('nickname');
    expect((await removed).isUnset, isTrue);
    expect(settings.getString('nickname'), isNull);

    settings.dispose();
  });

  test('describes the schema for a settings UI', () async {
    final settings = await open();
    final describe = settings.describe;

    expect(describe.namespace, 'org.mechanix.demo');
    expect(describe.className, 'app');
    expect(describe.version, 1);
    expect(describe.groups.single.id, 'colors');
    expect(describe.groups.single.label, 'Colors');
    expect(describe.keys, hasLength(5));

    final mode = describe.keys.firstWhere((key) => key.key == 'colors.mode');
    expect(mode.ty, 'enum');
    expect(mode.values, ['light', 'dark']);
    expect(mode.defaultToml, '"dark"');
    expect(mode.label, 'Mode');

    final radius =
        describe.keys.firstWhere((key) => key.key == 'colors.corner_radius');
    expect(radius.min, 0);
    expect(radius.max, 64);
    expect(radius.env, isNull);
    expect(describe.keys.firstWhere((key) => key.key == 'verbose').env,
        'MXCONFIG_DART_TEST_VERBOSE');

    expect(describe.keysOf('colors').map((key) => key.key),
        contains('colors.accent'));

    settings.dispose();
  });

  test('reports an unknown key without failing', () async {
    File('${dir.path}/org.mechanix.demo/config.toml')
      ..parent.createSync(recursive: true)
      ..writeAsStringSync('[colors]\nbogus = 1\naccent = "#111111"\n');
    final settings = await open();

    expect(settings.getString('colors.accent'), '#111111');
    final diagnostics = settings.diagnostics;
    expect(diagnostics, hasLength(1));
    expect(diagnostics.single.key, 'colors.bogus');
    expect(diagnostics.single.severity, 'warning');
    expect(diagnostics.single.isError, isFalse);

    settings.dispose();
  });

  test('a manifest gates the handle', () async {
    final settings = await open();

    settings.authorize(manifest, write: false);
    expect(() => settings.authorize(manifest, write: true), throwsA(anything));
    expect(() => settings.authorize(otherManifest), throwsA(anything));

    settings.dispose();
  });

  test('an edit made outside arrives on watch', () async {
    final settings = await open();
    final changed = settings.watch().first;

    final file = File('${dir.path}/org.mechanix.demo/config.toml');
    file.parent.createSync(recursive: true);
    file.writeAsStringSync('[colors]\naccent = "#00ff00"\n');

    final change = await changed.timeout(const Duration(seconds: 10));
    expect(change.key, 'colors.accent');
    expect(change.value, '"#00ff00"');
    expect(settings.getString('colors.accent'), '#00ff00');
    settings.dispose();
  });

  test('a version conflict names the other version', () async {
    final held = await open();
    final newer = schema.replaceFirst('version = 1', 'version = 2');
    await expectLater(
        Settings.openIn(userDir: dir.path, schemaToml: newer),
        throwsA(isA<MxconfigException>()
            .having((e) => e.versionConflict, 'versionConflict', isTrue)
            .having((e) => e.otherVersion, 'otherVersion', 1)));
    held.dispose();
  });

  test('another version reading untouched keys only reads', () async {
    final held = await open();
    // v2 renames `colors.accent`, and this build reads only `verbose`.
    const reader = '''
namespace = "org.mechanix.demo"
version = 2

[keys.verbose]
type = "bool"
default = false
label = "Verbose"
description = "Verbose logging."

[[migration]]
from = 1
to = 2
ops = [{ op = "rename", from = "colors.accent", to = "colors.color" }]
''';
    final settings =
        await Settings.openIn(userDir: dir.path, schemaToml: reader);
    expect(settings.otherVersion, 1);
    expect(settings.getBool('verbose'), isFalse);
    await expectLater(
        settings.setBool('verbose', true),
        throwsA(isA<MxconfigException>()
            .having((e) => e.versionConflict, 'versionConflict', isTrue)));
    settings.dispose();
    held.dispose();
  });
}
