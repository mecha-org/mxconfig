// A private bus and mxconfigd for the tests.

import 'dart:convert';
import 'dart:io';

import 'package:dbus/dbus.dart';

/// mxconfigd's bus name.
const busName = 'org.mechanix.MxConfig';

/// Start a private session bus. Returns it and its address.
Future<(Process, String)> startBus() async {
  final bus =
      await Process.start('dbus-daemon', ['--session', '--print-address']);
  final address = (await bus.stdout
          .transform(utf8.decoder)
          .transform(const LineSplitter())
          .first)
      .trim();
  return (bus, address);
}

/// The app the tests are, whose manifest grants the desktop namespaces they open.
const testApp = 'org.example.tool';

/// Start mxconfigd at [path] over [dir], and wait until [client] sees its name.
Future<Process> startDaemon(
    String path, String address, Directory dir, DBusClient client) async {
  final grants = [
    'demo',
    'art',
    'nothing',
    'late'
  ].map((name) => '\n[[settings]]\nnamespace = "org.mechanix.desktop.$name"\n'
      'access = "read-write"\n');
  File('${dir.path}/manifests/$testApp.toml')
    ..createSync(recursive: true)
    ..writeAsStringSync('client = "$testApp"\n${grants.join()}');
  final daemon = await Process.start(path, [], environment: {
    'DBUS_SESSION_BUS_ADDRESS': address,
    'MXCONFIG_SCHEMA_DIR': '${dir.path}/schemas',
    'MXCONFIG_USER_DIR': '${dir.path}/user',
    'MXCONFIG_VENDOR_DIR': '${dir.path}/none',
    'MXCONFIG_ADMIN_DIR': '${dir.path}/none',
    'MXCONFIG_MANIFEST_DIR': '${dir.path}/manifests',
  });
  final deadline = DateTime.now().add(const Duration(seconds: 10));
  while (!(await client.listNames()).contains(busName)) {
    if (DateTime.now().isAfter(deadline)) throw StateError('no daemon');
    await Future<void>.delayed(const Duration(milliseconds: 20));
  }
  return daemon;
}
