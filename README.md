# mxconfig

Typed, versioned TOML settings for apps. One schema gives a typed Rust API and a typed Dart API.

An app opens its namespace, reads and writes typed values, and hears every change, without dealing with files, layers, migrations or D-Bus. The library is an event source the app's runtime drives: it watches files with inotify, and reaches a daemon only when the runtime carries a bus. It never blocks, spawns threads or polls.

## Layout

| Path | What it is |
| --- | --- |
| `crates/mxconfig` | The library, and `Source`, the event source a runtime drives |
| `crates/mxconfig-schema` | The schema model the core and the generator share |
| `crates/mxconfig-codegen` | Typed Rust and Dart clients from a schema |
| `crates/mxconfig-macros` | `client!`, which `mxconfig` re-exports |
| `crates/mxconfig-example` | A plain Rust client, with tests |
| `crates/mxconfig-cli` | `mxconfig`: `check`, `dump`, `describe`, `assets`, `prune`, `set`, `unset` |
| `crates/mxconfig-daemon` | `mxconfigd`, which serves namespaces over D-Bus |
| `crates/mxconfig-ffi` | The `flutter_rust_bridge` layer under the Dart package |
| `packages/mxconfig_dart` | The Dart package, with no D-Bus code |
| `packages/mxconfig_dbus` | A transport for the Dart package over the app's `package:dbus` client |
| `data` | Install files for the daemon: D-Bus policy, polkit action, units |

An app that reads its own settings needs only `mxconfig`, and only its crate root, the stable app API. See [Public and internal API](docs/reference.md#public-and-internal-api).

## Quick start (Rust)

A schema:

```toml
# schema/org.mechanix.demo.toml
namespace = "org.mechanix.demo"
version = 1

[groups.colors]
label = "Colors"

[keys.colors.accent]
type = "string"
default = "#7aa2f7"
label = "Accent"
description = "The color the app highlights with."
```

Generate the client, at compile time:

```rust
// src/settings.rs
mxconfig::client!("schema/org.mechanix.demo.toml");
```

Open it, and handle its events:

```rust
use mxconfig::{Event, Source};

let source = Source::new();
source.request::<Demo>();

fn on_demo(event: Event<Demo>) -> Result<(), mxconfig::Error> {
    match event {
        Event::Opened(demo) => {
            demo.colors().set_accent("#ff0000")?;
        }
        Event::Changed(DemoChange::ColorsAccent(accent)) => println!("accent is now {accent}"),
        Event::OpenFailed(e) | Event::WriteFailed(e) => return Err(e),
        _ => {}
    }
    Ok(())
}
```

The app's runtime calls `on_demo` for each event. See [getting started](docs/getting-started.md#use-it-from-rust).

`crates/mxconfig-example` is the reference client.

## Quick start (Dart)

```sh
mxconfig-codegen dart schema/org.mechanix.demo.toml lib/generated
```

```dart
import 'generated/demo.dart';

final demo = await Demo.open();
print(demo.colors.accent);
await demo.colors.setAccent('#ff0000');
demo.changes().listen(print);
```

## Verify

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets
cargo test --workspace
cargo run -p mxconfig-cli -- check

export MXCONFIG_LIB=$PWD/target/debug/libmxconfig.so MXCONFIGD=$PWD/target/debug/mxconfigd
(cd packages/mxconfig_dart && dart analyze && dart test)
(cd packages/mxconfig_dbus && dart analyze && dart test)   # needs dbus-daemon
```

`crates/mxconfig/tests/public-api.txt` is the public API, checked by a test that needs a nightly toolchain. After a deliberate change, rerun it with `UPDATE_PUBLIC_API=1` and commit the new listing.

Regenerating the Dart bridge needs `flutter_rust_bridge_codegen` 2.13.0, run as `flutter_rust_bridge_codegen generate` from the repository root.

## Docs

- [`docs/getting-started.md`](docs/getting-started.md): write a schema, use it from Rust or Dart, and ship it.
- [`docs/reference.md`](docs/reference.md): schemas, classes, the generated API, assets, manifests, files and migrations.
- [`docs/daemon.md`](docs/daemon.md): backends, schema versions, the privileged service and the CLI.
- [`docs/runtimes.md`](docs/runtimes.md): driving a source and carrying the bus, for runtime and transport authors.
- [`docs/protocol.md`](docs/protocol.md): the D-Bus interface.

## License

BSD 3-Clause. See [LICENSE](LICENSE).
