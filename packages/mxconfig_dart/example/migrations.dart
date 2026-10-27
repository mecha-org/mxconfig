// A v1 file upgrades when a v2 client opens it.
import 'dart:io';

import 'package:mxconfig_dart/mxconfig.dart';

import 'generated/demo.dart';

Future<void> main() async {
  final dir = Directory.systemTemp.createTempSync('mxconfig-example');
  final file = File('${dir.path}/org.mechanix.demo/config.toml')
    ..parent.createSync(recursive: true)
    ..writeAsStringSync('[colors]\nradius = 12\n');
  File('${file.path}.state').writeAsStringSync('version = 1\n');
  stdout.writeln('before:\n${file.readAsStringSync().trim()}');

  final demo = Demo(await Settings.openIn(
    userDir: dir.path,
    schemaToml: schema,
  ));

  stdout.writeln('version: ${demo.settings.describe.version}');
  stdout
      .writeln('cornerRadius: ${demo.colors.cornerRadius} (was colors.radius)');
  stdout.writeln('mode: ${demo.colors.mode.value} (a new key, so its default)');
  stdout.writeln('after:\n${file.readAsStringSync().trim()}');
  // The last handle going stops the file watches, so the program can end.
  demo.settings.dispose();
  dir.deleteSync(recursive: true);
}
