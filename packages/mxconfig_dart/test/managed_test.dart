// Tests managed keys and assets through the Dart API.
import 'dart:io';
import 'dart:typed_data';

import 'package:mxconfig_dart/mxconfig.dart';
import 'package:test/test.dart';

const schema = '''
namespace = "org.mechanix.demo"
version = 1

[assets.layout]
path = "layout.yaml"
managed = true

[assets.theme]
path = "theme.css"

[keys.brightness]
type = "integer"
default = 50
label = "Brightness"
description = "Panel brightness."
managed = true

[keys.volume]
type = "integer"
default = 50
label = "Volume"
description = "Output volume."
''';

void main() {
  late Directory dir;

  setUp(() => dir = Directory.systemTemp.createTempSync('mxconfig-managed'));
  tearDown(() => dir.deleteSync(recursive: true));

  Future<Settings> open() =>
      Settings.openIn(userDir: dir.path, schemaToml: schema);

  File user(String name) => File('${dir.path}/org.mechanix.demo/$name');

  test('a managed key is described as managed', () async {
    final settings = await open();
    final keys = settings.describe.keys;
    expect(keys.firstWhere((key) => key.key == 'brightness').managed, isTrue);
    expect(keys.firstWhere((key) => key.key == 'volume').managed, isFalse);
    settings.dispose();
  });

  test('a managed key refuses every write', () async {
    final settings = await open();

    final refused = throwsA(isA<MxconfigException>());
    await expectLater(settings.setInt('brightness', 1), refused);
    await expectLater(settings.setToml('brightness', '1'), refused);
    await expectLater(settings.remove('brightness'), refused);
    expect(user('config.toml').existsSync(), isFalse);
    expect(settings.getInt('brightness'), 50);

    // An unmanaged key beside it writes as usual.
    await settings.setInt('volume', 10);
    expect(settings.getInt('volume'), 10);
    settings.dispose();
  });

  test('a managed asset refuses every write', () async {
    final settings = await open();

    final refused = throwsA(isA<MxconfigException>());
    final bytes = Uint8List.fromList([1, 2, 3]);
    await expectLater(settings.setAsset('layout', bytes), refused);
    await expectLater(settings.removeAsset('layout'), refused);
    expect(user('layout.yaml').existsSync(), isFalse);

    await settings.setAsset('theme', bytes);
    expect(settings.asset('theme'), bytes);
    settings.dispose();
  });

  test('the user value of a managed key is ignored with a warning', () async {
    user('config.toml')
      ..parent.createSync(recursive: true)
      ..writeAsStringSync('brightness = 99\nvolume = 70\n');
    user('layout.yaml').writeAsStringSync('user');
    final settings = await open();

    expect(settings.getInt('brightness'), 50);
    expect(settings.getInt('volume'), 70);
    expect(settings.asset('layout'), isNull);
    final note = settings.diagnostics.singleWhere((d) => d.key == 'brightness');
    expect(note.severity, 'warning');
    expect(note.message, contains('managed'));
    settings.dispose();
  });
}
