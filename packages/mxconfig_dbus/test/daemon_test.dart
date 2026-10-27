// Tests the daemon path via package:dbus. Needs MXCONFIGD.

import 'dart:convert';
import 'dart:io';

import 'package:dbus/dbus.dart';
import 'package:mxconfig_dart/mxconfig.dart';
import 'package:mxconfig_dbus/mxconfig_dbus.dart';
import 'package:test/test.dart';

import 'support.dart';

const schema = '''
namespace = "org.mechanix.desktop.demo"
version = 1

[keys.accent]
type = "string"
default = "#7aa2f7"
label = "Accent"
description = "d"

[assets.wallpaper]
path = "wallpaper.txt"
''';

/// The schema of `org.mechanix.desktop.<name>`.
String schemaOf(String name) =>
    schema.replaceFirst('desktop.demo', 'desktop.$name');

/// Open `org.mechanix.desktop.<name>` with its schema.
Future<Settings> open(String name) =>
    Settings.open('org.mechanix.desktop.$name', schema: schemaOf(name));

void main() {
  final daemonPath = Platform.environment['MXCONFIGD'];
  group('through mxconfigd',
      skip: daemonPath == null ? 'MXCONFIGD is not set' : false, () {
    late Directory dir;
    late Process bus;
    late Process daemon;
    late DBusClient client;
    late DBusTransport transport;

    setUpAll(() async {
      dir = await Directory.systemTemp.createTemp('mxconfig_daemon_');
      await Directory('${dir.path}/schemas').create();
      await File('${dir.path}/schemas/org.mechanix.desktop.demo.toml')
          .writeAsString(schema);
      // More namespaces, each first opened by one test.
      for (final name in ['art', 'late']) {
        await File('${dir.path}/schemas/org.mechanix.desktop.$name.toml')
            .writeAsString(schemaOf(name));
      }

      final String address;
      (bus, address) = await startBus();
      client = DBusClient(DBusAddress(address));
      daemon = await startDaemon(daemonPath!, address, dir, client);
      transport = await DBusTransport.install(client);
      Settings.appId = testApp;
    });

    tearDownAll(() async {
      await client.close();
      daemon.kill();
      bus.kill();
      await dir.delete(recursive: true);
    });

    test('a running daemon is seen before anything opens', () async {
      expect(await Settings.daemon(), DaemonPresence.running);
    });

    test(
        'a handle opened through the daemon reads, writes and hears other writers',
        () async {
      final settings = await open('demo');
      expect(settings.fromDaemon, isTrue);
      expect(settings.getString('accent'), '#7aa2f7');

      // A write completes once the daemon has applied it.
      await settings.setString('accent', '#abcdef');
      expect(settings.getString('accent'), '#abcdef');
      expect(
          File('${dir.path}/user/mxconfig/org.mechanix.desktop.demo/conf.toml')
              .readAsStringSync(),
          contains('#abcdef'));

      final other = await open('demo');
      final heard = settings.watch().firstWhere((c) => c.value == '"#010203"');
      await other.setString('accent', '#010203');
      await heard.timeout(const Duration(seconds: 5));
      expect(settings.getString('accent'), '#010203');
    });

    test('an asset arrives as the file the daemon opened, and follows it',
        () async {
      final home =
          Directory('${dir.path}/user/mxconfig/org.mechanix.desktop.art');
      await home.create(recursive: true);
      await File('${home.path}/wallpaper.txt').writeAsString('sky');
      final settings = await open('art');
      // The subscription's reply carries the file.
      expect(utf8.decode(settings.asset('wallpaper')!), 'sky');

      // Each new file arrives with a signal. Written atomically.
      for (final text in ['dunes', 'waves']) {
        final moved = settings.watchAssets().first;
        final next = File('${home.path}/.wallpaper.tmp');
        await next.writeAsString(text);
        await next.rename('${home.path}/wallpaper.txt');
        expect(await moved.timeout(const Duration(seconds: 5)), 'wallpaper');
        expect(utf8.decode(settings.asset('wallpaper')!), text);
      }

      // Writes from bytes and from a file both reach the daemon.
      await settings.setAsset('wallpaper', utf8.encode('stars'));
      expect(utf8.decode(settings.asset('wallpaper')!), 'stars');
      final picked = File('${dir.path}/picked.txt')..writeAsStringSync('sand');
      await settings.setAssetFromFile('wallpaper', picked.path);
      expect(utf8.decode(settings.asset('wallpaper')!), 'sand');
      expect(File('${home.path}/wallpaper.txt').readAsStringSync(), 'sand');
      await settings.removeAsset('wallpaper');
      expect(settings.asset('wallpaper'), isNull);
    });

    test('a namespace the daemon does not serve opens on its defaults',
        () async {
      final settings = await open('nothing');
      expect(settings.onDefaults, isTrue);
      expect(settings.getString('accent'), '#7aa2f7');
    });

    // Runs last, as it closes the shared transport.
    test('a closed transport ends an open on its defaults, instead of hanging',
        () async {
      final opening = open('late');
      // Let the open send its first call.
      await Future<void>.delayed(Duration.zero);
      await transport.close();
      final settings = await opening.timeout(const Duration(seconds: 5));
      expect(settings.onDefaults, isTrue);
    });
  });
}
