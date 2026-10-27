# Reference

The details behind [getting started](getting-started.md).

- [Schemas](#schemas)
- [Namespaces and classes](#namespaces-and-classes)
- [Env overrides](#env-overrides)
- [The generated API](#the-generated-api)
- [Events and errors](#events-and-errors)
- [The Dart settings API](#the-dart-settings-api)
- [Public and internal API](#public-and-internal-api)
- [Assets](#assets)
- [Manifests](#manifests)
- [Where files live](#where-files-live)
- [Migrations](#migrations)

## Schemas

[Getting started](getting-started.md#write-a-schema) has a full example.

- **Types:** `string`, `integer`, `float`, `bool`, `datetime`, `array`, `map`, and `enum` with `values`. Arrays and maps may declare `element`.
- **Bounds:** `min`, `max` and `max_length` are checked on every write, and reported when a file breaks them.
- **Defaults:** a key without a `default` starts unset.
- **Groups:** `[keys.colors.accent]` is in group `colors`. `[keys.verbose]` is ungrouped. `order` sorts groups and keys, with ungrouped keys last. Every group needs a `label`.
- **Managed:** `managed = true` on a key or asset means only vendor and admin files set it. See [classes](#namespaces-and-classes).
- **Directory:** an app schema may set `dir = "mechanix-demo"` to name its config directory. It defaults to the namespace id.
- **Retired:** `[retired]` lists `keys` and asset file paths (`assets`) the schema no longer uses. They can't be declared again, and `mxconfig prune` removes them.

Install a schema as `<namespace>.toml` on the [schema search path](#where-files-live). `mxconfig check` with no arguments validates every installed schema.

## Namespaces and classes

The label after the owner decides the class:

| Id | Class | Who reaches it |
| --- | --- | --- |
| `org.example.player` | `app` | the app itself |
| `org.mechanix.desktop.theme` | `desktop` | apps whose manifest declares it |
| `org.mechanix.privileged.display` | `privileged` | apps whose manifest declares it. Writes need polkit. |

- The class comes from the id. It is never declared.
- `desktop` and `privileged` namespaces are called shared.
- A reserved label can't come first, last or twice.

**Privileged namespaces** are device-wide. They have no user layer, so only vendor and admin values apply. Only [the privileged service](daemon.md#the-privileged-service) writes them, into the admin layer, after polkit approves. Their assets come from installed files only.

**Managed keys and assets,** in any namespace, are set only by vendor and admin files:

- The library and both daemons refuse writes to them.
- Generated clients have no setter for them.
- A user file's value for one is ignored, with a warning.
- Their env var is never read.

## Env overrides

A key or asset may declare `env`. While that variable is set and not empty, it beats every layer:

```toml
[keys.output.scale]
type = "float"
default = 1.0
env = "MECHANIX_DEMO_SCALE"
```

- **How the text is read:** strings and enums as they are; numbers and datetimes bare; bools as `true`, `false`, `1` or `0`; arrays and maps as TOML.
- **A bad value** is reported as a diagnostic, and the layers apply instead. The same goes for an asset's env path that isn't a file.
- **Writes** still go to the user file, but the env value wins until it's unset.
- **Managed keys** never read env.
- **Through the daemon,** a namespace sees the daemon's environment, not the app's.

## The generated API

For `org.mechanix.demo`:

- **`Demo`**, the handle. It has one accessor per group, such as `colors()`, and `settings()`, the untyped `Settings` underneath, for the schema, revision and diagnostics. `Settings::from_daemon()` says whether its values come through the daemon.
- **`Colors`**, with `accent()`, `set_accent(..)` and `clear_accent()` for each key. A managed key only has the getter. Setters check the value at once and return a write ticket.
- **`DemoChange`**, one variant per key with its type, such as `ColorsAccent(String)`, plus `Asset<Id>` for each asset.
- **`ColorsMode`**, a real enum with `as_str()` and `parse()`.
- **Return types:** keys with a default return `T`. Keys without one return `Option<T>`, or `T?` in Dart. Arrays return `Vec<mxconfig::Value>`, and maps `mxconfig::Table`.
- **Per asset:** `asset_<id>()`, plus `set_asset_<id>()`, `set_asset_<id>_from()` and `clear_asset_<id>()` unless it's managed.

In Dart:

- Names are camelCase, and setters return futures.
- There is no file-descriptor asset setter.
- Enums have `value` and `parse()`.
- Each change class holds the new value as `value`, and prints as its key and value.

`packages/mxconfig_dart/example/generated/` holds the output for the example schema. A test checks it is up to date.

**Trimmed clients.** An app that reads only some keys lists them, and its client carries only those:

```rust
mxconfig::client!("schema/org.mechanix.desktop.theme.toml", keys = ["accent"]);
```

```sh
mxconfig-codegen dart schema/org.mechanix.desktop.theme.toml lib/generated --keys accent
```

Only keys are trimmed; every asset stays. A trimmed client keeps working with another schema version as long as that version's migrations leave its keys and the schema's assets alone. See [schema versions](daemon.md#schema-versions).

## Events and errors

`outcome.event::<Demo>()` turns an outcome into an `Event<Demo>`, or `None` if it's about another namespace:

| Event | When |
| --- | --- |
| `Opened(Demo)` | The namespace opened: in the same turn from files, or once the daemon replies. |
| `OpenFailed(error)` | The namespace didn't open. |
| `Changed(DemoChange)` | A value or asset changed, whoever changed it. |
| `Written` | A write landed. |
| `WriteFailed(error)` | A write was refused, or may not have landed. |
| `Detached(cause)` | The handle stopped following its namespace. It keeps its values and refuses writes. |
| `Diagnosed(diagnostics)` | The namespace has new problems: right after it opens, then after any change that brings new ones. Each is reported once. Log them. |

Under it, `source.outcomes()` returns every namespace's outcomes as plain `Outcome`s. Runtime integrations use them directly. An app that needs to know which write failed keeps the `Ticket` a setter returns, and finds its outcome with `Ticket::written`.

A handle detaches for one of two reasons:

- **`Detach::Moved`:** its file, or the daemon serving it, moved to an incompatible [schema version](daemon.md#schema-versions). Restarting the app picks that up.
- **`Detach::Closed`:** its transport was closed. Drop the handle and open the namespace again.

`Event`, `Outcome` and `ErrorKind` may gain variants, so a `match` on any of them needs a `_` arm.

`Error::kind()` says what went wrong:

| Kind | Meaning |
| --- | --- |
| `Denied` | Refused by a manifest, polkit or a managed key. |
| `Invalid` | A key, value or TOML text was rejected. |
| `Unknown` | No such schema, asset or served namespace. |
| `Unreachable(bus)` | The runtime can't reach that bus. |
| `VersionConflict` | Another schema version holds the namespace. |
| `Detached` | The handle stopped following its namespace. |
| `Unconfirmed` | A write was sent but got no answer, so it may have landed. |
| `Other` | Anything else, such as a file that can't be read. |

After `Unconfirmed`, compare the handle's values with the edits before writing again. A handle that stays attached follows what the daemon holds.

## The Dart settings API

Under each generated class is a `Settings`, as `demo.settings`. `Settings.open(namespace, schema:)` is what `Demo.open()` calls. `Settings.openIn` opens over one user directory, for tests and embedding.

```dart
final settings = demo.settings;  // or: await Settings.openIn(userDir: dir, schemaToml: schema)

settings.getString('colors.accent');   // also getInt, getDouble, getBool, getToml
await settings.setString('colors.accent', '#ff0000');
await settings.remove('colors.accent');

settings.asset('layout');              // Uint8List?, null when nothing sets it
await settings.setAsset('layout', bytes);
await settings.setAssetFromFile('layout', path);
await settings.removeAsset('layout');

settings.describe;
settings.diagnostics;
settings.watch();
settings.watchAssets();
settings.watchDetached();              // DetachCause.moved or .closed
settings.onDefaults;                   // a shared namespace still on its schema defaults
settings.fromDaemon;                   // its values come through the daemon
await Settings.daemon(bus: 'session'); // DaemonPresence.noBus, .noDaemon or .running
settings.dispose();
```

A refused write fails its future with an `MxconfigException`. Its `versionConflict`, `otherVersion`, `detached` and `unconfirmed` fields match the [error kinds](#events-and-errors).

## Public and internal API

| Rust | Dart | For | Stable |
| --- | --- | --- | --- |
| the crate root | `package:mxconfig_dart/mxconfig.dart` | apps | yes |
| `mxconfig::runtime` | `package:mxconfig_dart/runtime.dart` | runtimes and transports | yes, but may still change before the first release |
| `mxconfig::host`, and anything hidden from the docs | | mxconfig's own daemon, CLI and Dart bridge | no |

Apps use only the first row. Hidden items, such as `Source::with_roots` or `Settings::prune`, belong to the last.

## Assets

An asset is a file the app uses, such as a wallpaper. It lives in the namespace's directory and goes through the same layers as values:

```toml
[assets.layout]
path = "layout.yaml"          # relative to the namespace directory
env = "MECHANIX_DEMO_LAYOUT"  # optional: an absolute path that wins
```

```rust
if let Some(asset) = settings.asset("layout") {
    let text = asset.text()?;            // asset.origin says which layer
}
settings.write_asset("layout", bytes)?;  // the user's copy
settings.write_asset_from("layout", fd)?;
settings.clear_asset("layout")?;         // a lower layer applies again
```

- Every write, and any edit made outside the app, arrives as `Event::Changed`, such as `DemoChange::AssetLayout`.
- When no layer has the file, `asset()` returns `None`, and the app falls back to what it embeds.
- A file from outside arrives as an fd, never as a path to keep. A sandboxed app picks one through the file-chooser portal and passes its fd to `write_asset_from`, which makes a copy the user owns.
- Through the daemon, the fd is passed over D-Bus unread. It must be a regular file of at most 64 MiB.

## Manifests

A manifest declares what an app may read or write besides its own namespace:

```toml
client = "org.example.player"

[[settings]]
namespace = "org.mechanix.desktop.theme"
access = "read"

[[settings]]
namespace = "org.mechanix.privileged.display"
access = "read-write"
temporary = true
```

- It's installed as `/usr/share/mxconfig/manifests/<app id>.toml`.
- `access` is `read` or `read-write`. A namespace without an entry is refused.
- An app reaches its own app namespace without a manifest.
- `temporary = true` works at runtime but fails `mxconfig check`, so it can't ship. With both a lasting and a temporary entry for one namespace, the stronger one applies.

**Who checks it:**

- **The daemon** checks it on every call.
- **Inside the app,** `with_manifest` (Rust) and `authorize` (Dart) check it too. They're the only check when the app reads files directly.

**How the daemon knows which app is calling:**

- **A Flatpak app** is known by its sandbox.
- **An app that opens exactly one app namespace of its own** is that app. Nothing to set.
- **Any other app** names itself once, before opening shared settings: `source.set_app("org.example.player")` in Rust, or `Settings.appId = 'org.example.player'` in Dart. Its own app namespaces stay its own.
- **An app with no id** is refused everywhere.

## Where files live

Each namespace is read from three layers:

| Layer, lowest first | Base | Override |
| --- | --- | --- |
| Vendor, in the OS image | `/usr/lib` | `MXCONFIG_VENDOR_DIR` |
| Admin | `/etc` | `MXCONFIG_ADMIN_DIR` |
| User | `$XDG_CONFIG_HOME` | `MXCONFIG_USER_DIR` |

| Other | Default | Override |
| --- | --- | --- |
| Schemas | `mxconfig/schemas` under `$XDG_DATA_HOME`, then each `$XDG_DATA_DIRS` entry | `MXCONFIG_SCHEMA_DIR`, `:`-separated |
| Manifests | `/usr/share/mxconfig/manifests` | `MXCONFIG_MANIFEST_DIR`, for tests |

- System layers follow the [UAPI configuration spec](https://uapi-group.org/specifications/specs/configuration_files_specification/). The user layer and schemas follow [XDG](https://specifications.freedesktop.org/basedir-spec/latest/).
- Overrides must be absolute paths.
- [Env overrides](#env-overrides) sit above every layer.

An app namespace has its own directory under each base. A shared namespace lives in mxconfig's:

```
<base>/<dir>/config.toml                   # an app
<base>/<dir>/config.d/*.toml
<base>/mxconfig/<namespace>/conf.toml      # a shared namespace
<base>/mxconfig/<namespace>/conf.d/*.toml
```

Assets sit in the same directory. Any of these files may be a symlink, as dotfile managers make them, and edits to its target are followed too. The daemon uses the same directories, so an app and the daemon always see the same files.

**Order.** Files apply in this order:

1. The vendor and admin main files, key by key.
2. The system drop-ins, merged by name across those layers and applied in name order. A higher layer's drop-in replaces a same-named lower one. An empty file, or a symlink to `/dev/null`, masks it.
3. The user's main file, then the user's drop-ins.

Main files merge key by key rather than replacing each other, so a vendor key can be overridden but not removed.

**Sidecar files.** Next to the user's main file:

- `.state` records the schema version, from the first write on, and recent migrations.
- `.lastgood` is the last copy that parsed. A user file that fails to parse is read from it instead, with an error diagnostic, and the broken text is copied to `.broken`.

A broken file in another layer is skipped, with an error diagnostic.

**Writes** are atomic and locked, and sync the file once. A power loss can lose the latest change, but never leaves a broken file.

**Watching.** Only the directories a namespace reads are watched, never `~/.config` or `/etc` themselves. The vendor layer isn't watched at all, since it only changes with an OS update. An app sees an admin directory created while it runs once it restarts. A daemon watches `/etc` for that one entry meanwhile.

**Flatpak apps:**

- The user layer is the sandbox's, under `~/.var/app/<app id>/config`.
- Inside a Flatpak, the library reads only the app's own directories, and the `MXCONFIG_*_DIR` overrides don't apply.
- The vendor layer is the app's own prefix: `/app/lib` stands in for `/usr/lib`.
- There is no admin layer. `/etc` inside the sandbox is the runtime's, not the host's.

## Migrations

The version only changes when a change alters what existing files mean. Adding or dropping a key or an asset needs no migration:

- **A new key** reads as its default wherever a file doesn't set it.
- **A dropped key** stays in the user's files, with a warning, until `mxconfig prune` removes it. A dropped asset's file stays, silently.
- List dropped keys and assets under `[retired]`, so they're never declared again with another meaning, and so prune also removes a dropped asset's file.

Everything else is a migration:

```toml
[[migration]]
from = 1
to = 2
ops = [
  { op = "rename", from = "colors.radius", to = "colors.corner_radius" },
]
```

| Op | Up | Down |
| --- | --- | --- |
| `rename` | moves the value | moves it back |
| `retype` | converts integer and float, or string and integer, float or bool | converts back |
| `remap` | maps a string value | maps it back, lossy if two mapped to one |
| `rename-asset` | moves an asset file | moves it back |

**When it runs.** Opening brings a user main file that has a `.state` up to the schema's version. One without a `.state` is taken as current.

- **Going up,** a value that can't be retyped, or an asset that can't be moved, fails the open. A missing key or unmapped value only warns.
- **Going down,** the last three steps recorded in `.state` are replayed in reverse, with a warning where a value can't convert back.

**What migrates.** Only the user's main file, and a privileged namespace's admin file in the privileged service. Vendor and admin files, and user drop-ins, are never rewritten: whoever ships them updates them with the schema. Until then, a file using an old key or asset path gets a warning naming the current one.

**What a version doesn't capture:**

- **A new key and a running app.** Adding a key doesn't change the version, so an app built before the key ignores it until it's rebuilt and restarted. A settings app can set the key meanwhile, and nothing in the running app changes.
- **Pruning across builds.** `prune` is refused while another version holds the namespace. Otherwise it removes whatever the installed schema doesn't declare, including keys an app build that isn't running still reads. Run it deliberately.

`crates/mxconfig-example/tests` and `packages/mxconfig_dart/example/migrations.dart` migrate a real file.
