# Getting started

Write a schema, use it from Rust or Dart, and ship it. The details are in [the reference](reference.md).

- [Write a schema](#write-a-schema)
- [Use it from Rust](#use-it-from-rust)
- [Use it from Dart or Flutter](#use-it-from-dart-or-flutter)
- [Ship it](#ship-it)

## Write a schema

A schema is a TOML file. It names the namespace, gives its version, and declares each key with the text a settings UI shows.

```toml
namespace = "org.mechanix.demo"
version = 1

[groups.colors]
label = "Colors"
order = 10

[keys.colors.accent]
type = "string"
default = "#7aa2f7"
label = "Accent"
description = "The color the app highlights with."

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
description = "Light or dark."

[keys.verbose]
type = "bool"
default = false
label = "Verbose"
description = "Log extra detail."
```

- An id like `org.mechanix.demo` is the app's own namespace. Ids with `desktop` or `privileged` in them are shared between apps. See [classes](reference.md#namespaces-and-classes).
- `version` starts at `1`. It only changes with a [migration](reference.md#migrations).
- Every key needs a `label` and a `description`.

Keep the schema in the app's repository. Run `mxconfig check schema/org.mechanix.demo.toml` to validate it.

## Use it from Rust

Generate the client at compile time. The path is relative to the crate root:

```rust
// src/settings.rs
mxconfig::client!("schema/org.mechanix.demo.toml");
```

This gives `Demo`, with one accessor per group, such as `demo.colors()`, and a getter and setter per key. See [the generated API](reference.md#the-generated-api).

Ask for the namespace through the app's one `mxconfig::Source`, and handle its events in one function:

```rust
use mxconfig::{Event, Source};

let source = Source::new();
source.request::<Demo>();

fn on_demo(app: &mut App, event: Event<Demo>) -> Result<(), mxconfig::Error> {
    match event {
        Event::Opened(demo) => {
            println!("accent is {}", demo.colors().accent());
            demo.colors().set_accent("#ff0000")?;
            app.demo = Some(demo);
        }
        Event::Changed(DemoChange::ColorsAccent(accent)) => println!("accent is now {accent}"),
        Event::OpenFailed(e) | Event::WriteFailed(e) => return Err(e),
        _ => {}
    }
    Ok(())
}
```

`Event<Demo>` brings everything about the namespace: `Opened` with the handle, which the app keeps, or `OpenFailed`; `Changed` with a typed `DemoChange`; `Written` or `WriteFailed` for each write; `Detached`; and `Diagnosed` with new problems to log.

A runtime integration calls the handler for each event. Without one, this goes after each wake-up of the app's loop, and after app code runs:

```rust
for outcome in source.outcomes() {
    if let Some(event) = outcome.event::<Demo>() {
        on_demo(&mut app, event)?;
    }
}
```

What to know:

- **Getters return at once. Writes finish later.** A setter checks the value right away, and `Event::Written` or `Event::WriteFailed` says whether the write landed.
- **Every change arrives the same way.** Your own writes, other apps' writes, hand edits and migrations all arrive as `Event::Changed`.
- **Open once.** Opening a namespace again returns the same handle. The namespace closes when its last handle is dropped.
- **The app reads with its own schema.** The client carries the schema it was built from, so the app works even if no schema is installed.
- **Opening works the same everywhere.** From files, `Opened` arrives in the same turn. Through the daemon, it arrives once the daemon replies. So adding a transport later needs no other change. See [backends](daemon.md#backends).

Every event and error kind is listed in [events and errors](reference.md#events-and-errors). To drive the source from your own event loop, see [runtimes](runtimes.md).

## Use it from Dart or Flutter

These steps assume an mxconfig checkout next to the app.

**1. Install the generator** once:

```sh
cargo install --path ../mxconfig/crates/mxconfig-codegen
```

**2. Add the packages** to `pubspec.yaml`:

```yaml
dependencies:
  mxconfig_dart:
    path: ../mxconfig/packages/mxconfig_dart
  # Only to reach mxconfigd, for shared namespaces:
  mxconfig_dbus:
    path: ../mxconfig/packages/mxconfig_dbus
  dbus: ">=0.7.14 <0.9.0"
```

**3. Generate the client** and commit it next to the schema:

```sh
mxconfig-codegen dart schema/org.mechanix.demo.toml lib/generated
```

This writes `lib/generated/demo.dart`, named after the last part of the namespace. Run it again whenever the schema changes. With no directory, the generator prints the client instead, so a test can catch a forgotten run:

```dart
test('the generated client matches the schema', () async {
  final run = await Process.run(
      'mxconfig-codegen', ['dart', 'schema/org.mechanix.demo.toml']);
  expect(run.stdout, File('lib/generated/demo.dart').readAsStringSync());
});
```

**4. Build the native library:**

```sh
cargo build --release -p mxconfig-ffi   # target/release/libmxconfig.so
```

The package finds it through `libraryPath`, then the `MXCONFIG_LIB` env var, then the system's library path. A Flutter app can instead ship it in its bundle's `lib/` directory, installed there from `linux/CMakeLists.txt`.

**5. Use it:**

```dart
import 'generated/demo.dart';

final demo = await Demo.open();
final colors = demo.colors;

colors.accent;                       // '#7aa2f7', or the user's value
await colors.setAccent('#ff0000');   // completes once the write lands

await for (final change in demo.changes()) {
  switch (change) {
    case ColorsModeChange(:final value): print('mode is $value');
    default:
  }
}
```

- `changes()` gives one class per key, and every change, the app's own writes included. Printing a change shows its key and value, such as `colors.mode = ColorsMode.light`.
- A refused write fails its future with an `MxconfigException`.
- Problems the library finds are logged to stderr.

**6. To read shared settings,** install the D-Bus transport once, over the app's own client:

```dart
import 'package:dbus/dbus.dart';
import 'package:mxconfig_dbus/mxconfig_dbus.dart';

await DBusTransport.install(DBusClient.session());
```

Without it, shared namespaces hold their schema defaults. An app that reads only its own namespace doesn't need it. `close()` lets go of the bus and leaves the client open.

An app that reads shared settings also needs a [manifest](reference.md#manifests). If it doesn't open exactly one app namespace of its own, it also sets `Settings.appId` before its first open.

`packages/mxconfig_dart/example/demo.dart` is a runnable program that does all of this. It reads a shared namespace, `org.mechanix.desktop.display`, through `mxconfigd`, as its manifest `crates/mxconfig-example/manifest/org.mechanix.demo.toml` grants. The untyped API under the generated class is in [the Dart settings API](reference.md#the-dart-settings-api).

## Ship it

What an app's package installs, for `org.mechanix.demo` with `dir = "mechanix-demo"` and a `layout` asset:

| File | Installed as | Needed |
| --- | --- | --- |
| The schema | `/usr/share/mxconfig/schemas/org.mechanix.demo.toml` | Always |
| Default values | `/usr/lib/mechanix-demo/config.toml` | Only if they differ from the schema's defaults |
| Default assets | `/usr/lib/mechanix-demo/layout.yaml` | For each asset the app needs a file for |
| A manifest | `/usr/share/mxconfig/manifests/<app id>.toml` | If the app uses shared namespaces, or another app's |

- **The schema.** Install the same file the client was generated from. The app itself doesn't need it installed, but the CLI, settings UIs and the daemon do.
- **Defaults.** The schema's defaults cover most keys. Ship a default file only for values that differ per device, image or build.
- **Flatpak.** Use the app's own prefix: the schema at `/app/share/mxconfig/schemas/<namespace>.toml`, and defaults and assets under `/app/lib/<dir>/`.
- **Shared namespaces** keep their defaults in `/usr/lib/mxconfig/<namespace>/conf.toml`. A privileged one also needs [the privileged service](daemon.md#the-privileged-service), which `data/` installs once for all apps.

Before a release, check the schema and the manifest. This fails on missing labels, a broken migration chain or a `temporary` grant:

```sh
mxconfig check schema/org.mechanix.demo.toml manifest/org.mechanix.demo.toml
```
