// Schema versions through mxconfigd, seen by Dart handles. Needs MXCONFIGD.

import 'dart:convert';
import 'dart:io';

import 'package:dbus/dbus.dart';
import 'package:mxconfig_dart/mxconfig.dart';
import 'package:mxconfig_dbus/mxconfig_dbus.dart';
import 'package:test/test.dart';

import 'support.dart';

/// [namespace] at [version]. v2 renames `volume` to `level`.
String schema(String namespace, int version) {
  final key = version == 1 ? 'volume' : 'level';
  final text = '''
namespace = "$namespace"
version = $version
backend = "daemon"

[keys.$key]
type = "integer"
default = 80
label = "Volume"
description = "d"

[keys.mute]
type = "bool"
default = false
label = "Mute"
description = "d"
''';
  if (version == 1) return text;
  return '$text\n[[migration]]\nfrom = 1\nto = 2\n'
      'ops = [{ op = "rename", from = "volume", to = "level" }]\n';
}

/// Holds a version on a lock file from another process: path, then version.
const holdLock = '''
import fcntl, os, struct, sys
fd = os.open(sys.argv[1], os.O_RDWR | os.O_CREAT, 0o600)
lock = struct.pack("hh4xqqi4x", fcntl.F_RDLCK, os.SEEK_SET, int(sys.argv[2]) + 1, 1, 0)
fcntl.fcntl(fd, fcntl.F_OFD_SETLK, lock)
print("held", flush=True)
sys.stdin.read()
''';

void main() {
  final daemonPath = Platform.environment['MXCONFIGD'];
  group('schema versions through mxconfigd',
      skip: daemonPath == null ? 'MXCONFIGD is not set' : false, () {
    late Directory dir;
    late Process bus;
    late String address;
    late DBusClient client;
    late DBusTransport transport;
    Process? daemon;

    void install(String namespace, String text) =>
        File('${dir.path}/schemas/$namespace.toml').writeAsStringSync(text);

    String userFile(String namespace) =>
        File('${dir.path}/user/$namespace/config.toml').readAsStringSync();

    /// Start mxconfigd and wait until it owns its name.
    Future<void> start() async =>
        daemon = await startDaemon(daemonPath!, address, dir, client);

    /// Stop mxconfigd, install [text] for [namespace], and start it again.
    Future<void> restart(String namespace, String text) async {
      daemon!.kill();
      await daemon!.exitCode;
      install(namespace, text);
      await start();
    }

    /// Wait until [settings] has subscribed again, which starts its revision over.
    Future<void> resubscribed(Settings settings, int before) async {
      final deadline = DateTime.now().add(const Duration(seconds: 10));
      while (settings.revision >= before) {
        if (DateTime.now().isAfter(deadline)) {
          throw StateError('not resubscribed');
        }
        await Future<void>.delayed(const Duration(milliseconds: 20));
      }
    }

    /// Subscribe at [version] once the daemon has noticed a closed subscription.
    Future<void> subscribeOnceReleased(String namespace, int version) async {
      for (var tries = 0;; tries++) {
        try {
          await client.callMethod(
              destination: busName,
              path: DBusObjectPath('/org/mechanix/MxConfig'),
              interface: busName,
              name: 'Subscribe',
              values: [
                DBusString(namespace),
                DBusString(schema(namespace, version)),
                DBusString(namespace)
              ]);
          return;
        } on DBusMethodResponseException {
          if (tries == 50) rethrow;
          await Future<void>.delayed(const Duration(milliseconds: 20));
        }
      }
    }

    setUpAll(() async {
      dir = await Directory.systemTemp.createTemp('mxconfig_schema_');
      await Directory('${dir.path}/schemas').create();
      for (final name in ['sound', 'tone', 'chime', 'bell', 'gong']) {
        install('org.mechanix.$name', schema('org.mechanix.$name', 1));
      }
      (bus, address) = await startBus();
      client = DBusClient(DBusAddress(address));
      await start();
      transport = await DBusTransport.install(client);
    });

    tearDownAll(() async {
      await client.close();
      daemon?.kill();
      bus.kill();
      await dir.delete(recursive: true);
    });

    test('a restart keeps serving an app its own schema', () async {
      const namespace = 'org.mechanix.sound';
      final settings =
          await Settings.open(namespace, schema: schema(namespace, 1));
      expect(settings.fromDaemon, isTrue);
      await settings.setInt('volume', 35);
      final before = settings.revision;

      // The daemon restarts with v2 installed, and the v1 handle subscribes again.
      await restart(namespace, schema(namespace, 2));
      await resubscribed(settings, before);

      // Its own schema is trusted, so every key works and the file stays on v1.
      expect(settings.getInt('volume'), 35);
      await settings.setInt('volume', 36);
      expect(settings.getInt('volume'), 36);
      expect(userFile(namespace), contains('volume = 36'));
    });

    test('an open another version holds is refused, and says so', () async {
      const namespace = 'org.mechanix.tone';
      // Another client holds the namespace at v2.
      await client.callMethod(
          destination: busName,
          path: DBusObjectPath('/org/mechanix/MxConfig'),
          interface: busName,
          name: 'Subscribe',
          values: [
            DBusString(namespace),
            DBusString(schema(namespace, 2)),
            DBusString(namespace)
          ]);

      await expectLater(
          Settings.open(namespace, schema: schema(namespace, 1)),
          throwsA(isA<MxconfigException>()
              .having((e) => e.versionConflict, 'versionConflict', isTrue)));
    });

    test('a handle detaches when another version takes over', () async {
      const namespace = 'org.mechanix.chime';
      final settings =
          await Settings.open(namespace, schema: schema(namespace, 1));
      await settings.setInt('volume', 35);
      final before = settings.revision;

      // A v2 process holds the namespace while the daemon restarts.
      final lock = '${dir.path}/user/$namespace.lock';
      final holder =
          await Process.start('python3', ['-c', holdLock, lock, '2']);
      final held = await holder.stdout
          .transform(utf8.decoder)
          .transform(const LineSplitter())
          .first;
      expect(held, 'held');
      final detached = settings.watchDetached().first;
      await restart(namespace, schema(namespace, 2));
      await detached.timeout(const Duration(seconds: 10));
      expect(settings.revision, before, reason: 'nothing new was taken');

      // It keeps the value it had, never a default, and refuses writes.
      expect(settings.getInt('volume'), 35);
      await expectLater(
          settings.setInt('volume', 10),
          throwsA(isA<MxconfigException>()
              .having((e) => e.detached, 'detached', isTrue)));
      holder.kill();
      await holder.exitCode;
    });

    test('disposing the last handle ends its subscription', () async {
      const namespace = 'org.mechanix.gong';
      final settings =
          await Settings.open(namespace, schema: schema(namespace, 1));
      await settings.setInt('volume', 35);
      settings.dispose();

      // Nothing holds v1 any more, so v2 opens and migrates the file.
      await subscribeOnceReleased(namespace, 2);
      expect(userFile(namespace), contains('level = 35'));
    });

    // Closes the transport, so it runs last.
    test('closing the transport releases the hold, and the handle detaches',
        () async {
      const namespace = 'org.mechanix.bell';
      final settings =
          await Settings.open(namespace, schema: schema(namespace, 1));
      await settings.setInt('volume', 35);
      final detached = settings.watchDetached().first;

      await transport.close();
      expect(await detached.timeout(const Duration(seconds: 10)),
          DetachCause.closed);
      expect(settings.getInt('volume'), 35, reason: 'it keeps its values');
      await expectLater(
          settings.setInt('volume', 10),
          throwsA(isA<MxconfigException>()
              .having((e) => e.detached, 'detached', isTrue)));

      // The hold is gone, so another version may now open the namespace.
      await subscribeOnceReleased(namespace, 2);
      expect(userFile(namespace), contains('level = 35'));
    });
  });
}
