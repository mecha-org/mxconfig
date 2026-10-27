# mxconfig_dbus

Carries [mxconfig_dart](../mxconfig_dart/README.md)'s D-Bus traffic to mxconfigd over a `package:dbus` client the app already has. An app that reads only its own namespace doesn't need it. Without it, shared namespaces hold their schema defaults.

```dart
import 'package:dbus/dbus.dart';
import 'package:mxconfig_dbus/mxconfig_dbus.dart';

final transport = await DBusTransport.install(DBusClient.session());
// Settings.open now reaches the daemon too.
await transport.close();
```

`close()` lets go of the bus and leaves the client open. See [getting started](../../docs/getting-started.md#use-it-from-dart-or-flutter).

## Testing

```sh
MXCONFIGD=$PWD/../../target/debug/mxconfigd MXCONFIG_LIB=../../target/debug/libmxconfig.so dart test
```

Build `mxconfig-ffi` and `mxconfig-daemon` first. The tests need `dbus-daemon`.
