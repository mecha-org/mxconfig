# The daemons and the CLI

How namespaces are served, how schema versions are kept apart, and the command-line tool.

- [Backends](#backends)
- [Shared namespaces on defaults](#shared-namespaces-on-defaults)
- [Schema versions](#schema-versions)
- [Who may reach what](#who-may-reach-what)
- [The privileged service](#the-privileged-service)
- [The CLI](#the-cli)

## Backends

| Namespace | Served by | Without its daemon |
| --- | --- | --- |
| app | its files, then `mxconfigd` | files alone, and the open fails only if they do |
| desktop | `mxconfigd` | schema defaults |
| privileged | the privileged service | schema defaults |

- **An app's own namespace** is read from its files, in the app's process. `mxconfigd` serves it only when the files can't be read or written.
- **A shared namespace** is only ever reached through its daemon. Apps never open shared files.
- **A daemon is only tried** once the runtime reaches its bus.
- **Falling back** to the other backend adds a warning to the handle's diagnostics. When both fail, the error gives both reasons.
- **A refusal or a rejected value** is an answer, so it never falls back.
- **For debugging,** `MXCONFIG_BACKEND=local` or `daemon` limits an app namespace to one backend.

When a namespace goes through the daemon:

- Opening is one `Subscribe` call, and `Opened` waits for the daemon's reply.
- Reads come from that reply, replaced by each newer one.
- A write is one atomic `Apply`. Its `Changed` arrives before `Written`.
- Assets arrive as fds the daemon opened.
- Migrations, pruning and env overrides happen in the daemon.

## Shared namespaces on defaults

An app's own values are never made up: without its files or daemon, its open fails.

A shared namespace is different. An app usually reads only cosmetic things from it, and must still run outside the Mecha desktop. So when its daemon can't serve it, the open succeeds on the schema's defaults. That happens when no transport carries the bus, no daemon is running, or the daemon doesn't serve that namespace. Then:

- `Settings::on_defaults()` (`onDefaults` in Dart) is true, and a diagnostic says so.
- Writes fail as `Unreachable` while the bus isn't carried, and as `VersionConflict` when the daemon serves an incompatible version.
- Once the bus is reached, or a daemon starts, the handle subscribes. The real values then arrive as ordinary `Changed` events.
- If the daemon serves another version whose migrations change the app's keys, the handle stays on its defaults, with an error diagnostic naming both versions.
- A refusal or a bad value still fails the open.

An app that needs the real values, such as a settings app, checks first. `Source::daemon(bus)` (`Settings.daemon(bus:)` in Dart) says `NoBus`, `NoDaemon` or `Running`.

## Schema versions

An app reads its settings only with the schema it was built with. So a namespace is held at one schema version at a time.

**Opening holds the version.** The handle locks its version in `<dir>.lock`, next to the namespace's directory in the user layer. It lets go with its last handle, and the kernel drops the lock if the process dies. A privileged namespace needs no lock, since only the privileged service writes it.

**The file migrates only when nothing else holds it.** An open that finds no other version migrates the file, up or down, then opens.

**While another version holds it,** an open either reads only or is refused:

- If the migrations between the two versions leave the app's keys and assets alone, it opens and reads normally. Writes fail with `ErrorKind::VersionConflict`. `Settings::other_version()` (`otherVersion` in Dart) names the version holding it, so a settings app can grey out its controls and say which app needs a restart.
- Otherwise the open fails with `ErrorKind::VersionConflict`, naming both versions. `Error::other_version()` gives the other version when it's known.
- Either way nothing migrates, and the running app isn't affected.

**The newer side's migrations decide.** A newer app uses its own. An older one reading files uses the steps the newer version recorded in `.state`, and is refused without them. Trimmed clients, which carry fewer keys, are the ones most likely to keep working.

**A file that can't be migrated also refuses the open.** That's a downgrade past the steps the file recorded, or a file that needs migrating when its lock can't be held. The file stays as it is.

**A detached handle keeps its values.** If the file moves to another version anyway, despite the locks, the handle stops following it. `Event::Detached(Detach::Moved)` arrives (`watchDetached()` in Dart). Its values stay, writes fail with `ErrorKind::Detached`, and it never falls back to defaults.

**The daemon follows the same rules:**

- It holds a namespace, and its lock, while anyone is subscribed.
- It returns the schema it serves with every `Subscribe`.
- An app's own namespace is opened on the app's schema, so a running app survives a daemon restart.
- For any other namespace, an app at another version compares the two schemas itself. It reads only if its keys are untouched, and otherwise treats it as a conflict. An app already open that then hits a conflict detaches.

The privileged service alone writes and migrates a privileged namespace's admin file. Tools such as the CLI only read it.

## Who may reach what

An app reaches its own app namespace, and anything else its [manifest](reference.md#manifests) grants. The daemon serves a Flatpak app's own namespace from the files the app itself uses. See [access](protocol.md#access) and [where an app namespace lives](protocol.md#where-an-app-namespace-lives).

## The privileged service

`mxconfigd --privileged` runs as root on the system bus. It serves `privileged` namespaces, reads and writes alike, and nothing else.

- It writes the admin layer, and polkit approves each write as `org.mechanix.mxconfig.write`.
- Apps reach it once the runtime reaches `Bus::System`.
- The session daemon refuses privileged namespaces.

```sh
mxconfigd                # installed and registered app schemas, and desktop ones, on the session bus
mxconfigd --privileged   # every installed privileged schema, on the system bus
```

`data/` holds the install files for both: the D-Bus policy, the polkit action, a tmpfiles.d entry, and a D-Bus activation file and systemd unit for each daemon. D-Bus starts each daemon on its first call.

The D-Bus interface is specified in [`protocol.md`](protocol.md).

## The CLI

| Command | Does |
| --- | --- |
| `mxconfig check [files]` | Validates schemas and manifests, every installed one by default. |
| `mxconfig dump <namespace>` | Prints each effective value and its layer. |
| `mxconfig describe <namespace>` | Prints the schema as a UI sees it. |
| `mxconfig assets <namespace>` | Lists assets and where they come from. |
| `mxconfig prune <namespace>` | Removes keys the schema doesn't define, and retired asset files, from the user's copy. |
| `mxconfig set <namespace> <key> <value>` | Sets a key in the user's copy. |
| `mxconfig unset <namespace> <key>` | Unsets a key in the user's copy, so a lower layer or the default applies. |

- **No daemon needed.** The CLI works on the files directly.
- **Flatpak apps.** Like the daemon, it uses a Flatpak app's own settings when the host has no schema for it. `--flatpak` uses them even when it does.
- **Overrides.** Every `MXCONFIG_*_DIR` override it's given must be an absolute path.
- **`set`** checks the value against the installed schema. Running apps and the daemon follow the change through their file watches.
- **Values** are TOML, such as `60`, `true` or `[1, 2]`. A string or enum key also takes plain text, so `mxconfig set org.mechanix.desktop.theme accent '#ff0000'` works.
- **Privileged namespaces** are refused, since only the privileged service writes them.
