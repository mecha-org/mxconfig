// Open the app's own settings and a shared one, print them, then print every
// change until Ctrl-C.
//
//   dart run example/demo.dart                # just watch
//   dart run example/demo.dart '#ff0000' 60   # write an accent and a brightness first
//
// While it runs, change a value and watch it arrive:
//   - run it again with other values, in a second terminal;
//   - or edit ~/.config/org.mechanix.demo/config.toml, e.g. `[colors]` `mode = "light"`.
//
// The brightness is shared, so it goes through mxconfigd. The daemon serves it
// once its schema is installed, as /usr/share/mxconfig/schemas/
// org.mechanix.desktop.display.toml. Until then it holds its default and refuses writes.
// The demo may use it once its manifest is installed, as
// /usr/share/mxconfig/manifests/org.mechanix.demo.toml.
import 'dart:io';

import 'package:dbus/dbus.dart';
import 'package:mxconfig_dart/mxconfig.dart';
import 'package:mxconfig_dbus/mxconfig_dbus.dart';

import 'generated/demo.dart';
import 'generated/display.dart';

Future<void> main(List<String> args) async {
  // Shared settings come from mxconfigd, over the app's own D-Bus connection.
  final bus = DBusClient.session();
  await DBusTransport.install(bus);

  final demo = await Demo.open();
  final display = await Display.open();
  final colors = demo.colors;
  print('accent ${colors.accent}, mode ${colors.mode.value}, '
      'brightness ${display.brightness}');

  // Every change arrives here: this app's writes, file edits and other apps.
  demo.changes().listen((change) {
    switch (change) {
      case ColorsAccentChange(:final value):
        print('accent is now $value');
      case ColorsModeChange(:final value):
        print('mode is now ${value.value}');
      default:
        print('${change.runtimeType}: ${change.value}');
    }
  });
  display.changes().listen((change) {
    if (change case BrightnessChange(:final value)) {
      print('brightness is now $value');
    }
  });

  // A write completes once it lands. Its change arrives above, like anyone else's.
  try {
    if (args case [final accent, ...]) await colors.setAccent(accent);
    if (args case [_, final brightness]) {
      await display.setBrightness(int.parse(brightness));
    }
  } on MxconfigException catch (e) {
    print('not written: ${e.message}');
  }

  print('watching; press Ctrl-C to stop');
  await ProcessSignal.sigint.watch().first;
  demo.settings.dispose();
  display.settings.dispose();
  await bus.close();
}
