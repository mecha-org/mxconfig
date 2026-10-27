# The mxconfig D-Bus protocol

The contract between `mxconfigd` and its clients. Every signature is in [`data/org.mechanix.MxConfig.xml`](../data/org.mechanix.MxConfig.xml), and a test checks the daemon serves exactly that.

mxconfig's clients use `Subscribe`, `Apply`, `WriteAsset` and `ClearAsset`, and listen for `Changed` and `AssetChanged`. `ListNamespaces` tells a caller what it may reach.

## Names

| | |
| --- | --- |
| Bus name | `org.mechanix.MxConfig` |
| Object path | `/org/mechanix/MxConfig` |
| Interface | `org.mechanix.MxConfig` |

- **Session bus:** `mxconfigd` serves every desktop namespace, and app namespaces to clients whose files can't serve them. It reads and writes the same files those apps would: an app namespace in the app's own directory, a desktop one in `~/.config/mxconfig/<namespace>/`.
- **System bus:** `mxconfigd --privileged` serves `privileged` namespaces, writing the admin layer behind polkit.

Clients never read a shared namespace's files. Each one is served by exactly one daemon, which refuses the others' namespaces as `UnknownObject`.

## Values

A value crosses the bus as one TOML value in a string, such as `35`, `"speakers"` or `[1, 2]`. The client decodes it with its schema.

- In `a{sv}` maps, each value is a variant holding that string.
- An entry is `(key, set, text)`. `set` is false when the key was removed or became unset, and `text` is then empty.

Text that is not one TOML value is refused with `InvalidArgs`.

## Methods

| Method | Who | What |
| --- | --- | --- |
| `ListNamespaces(s app) → as` | anyone | The namespaces this app may reach. |
| `Apply(s namespace, a(sbs) edits, s app) → (t, a{sv})` | writers | Applies all edits or none. Returns the revision and every value after it. |
| `Subscribe(s namespace, s schema, s app) → (h, s, t, a{sv}, a(ssh))` | readers | The subscription, then the schema text, revision, values and assets. Signals reach the caller while the subscription lasts. |
| `WriteAsset(h file, s namespace, s id, s app)` | writers | Copies `file` as the user's copy of an asset. |
| `ClearAsset(s namespace, s id, s app)` | writers | Removes the user's copy. |

Every method takes `app`, the app the caller says it is, such as `org.example.player`. [Access](#access) says how it is used.

### An app's own schema

A client that carries its schema passes the text to `Subscribe`, on every call, so a restarted daemon learns it again. A client without one passes an empty string. The text must parse and name the namespace.

- **The caller's own app namespace** is the one its `app` names. The session daemon registers the schema, and opens the namespace on it if nobody holds it. A registration replaces an earlier one only with a higher version. It is used for callers that pass none, if no schema is installed.
- **Any other namespace** is opened on the installed schema. The text only says which version the client reads with.

### Versions

While anyone is subscribed, the daemon holds a namespace open on the version it opened with, and the session daemon holds its version lock as an app reading its own files would. The privileged daemon is the only process that writes admin files, so it needs none. The next open after the last subscriber leaves takes the current schema, and migrates the file if no process holds another version. A call from a caller that isn't subscribed opens the namespace afresh.

A `Subscribe` whose schema has another version than the one served still gets the reply, with the served schema. The client compares the two: if the migrations between them leave every key and asset its schema declares alone, it reads and refuses its own writes, and otherwise it treats the reply as a `VersionConflict`. `Apply` carries no schema, so the daemon can't tell such a client apart. Any call that would open the namespace while a process holds another version is refused with `VersionConflict`. A client already open that hits a conflict on a resubscribe detaches: it keeps its values and refuses writes.

### Where an app namespace lives

The daemon serves an app namespace from the files the app itself uses. For a Flatpak app calling about its own namespace, those are its sandbox's directories, from the `[Instance]` paths in its `.flatpak-info`. For any other caller, they are the host's, if the host has the schema installed or a native app registered one. Otherwise they are the Flatpak app's, found with `flatpak info --show-location`. Each place has its own revisions and subscribers.

### Asset writes

The fd comes first because `package:dbus` misaligns an `h` after other arguments. It must be a regular file of at most 64 MiB, such as a memfd or a file from a document portal. Pipes and sockets are refused. The daemon reads from the start, whatever the offset. A managed asset refuses writes.

## Signals

```
Changed(s namespace, t revision, a{sv} values)
AssetChanged(s namespace, s id, a(sh) file)
```

`Changed` carries the revision and every set value after a change, like the `Subscribe` reply without schema and assets. A key missing from it is unset.

`AssetChanged` carries the asset's winning file as `(path, fd)`, or an empty array when nothing sets it. The path is for messages only. A client keeps the newest fd and reads it from the start.

Both are sent to each subscriber directly, not broadcast. A broadcast would reach every connection, sandboxed apps included, bypassing their manifests. Any peer can send a unicast signal, so a client asks the bus who owns `org.mechanix.MxConfig` with `GetNameOwner`, and takes signals only from that owner.

## Ordering and revisions

- Each namespace has a revision, raised by every change while it is open. It starts again when the namespace is next opened, which only happens once nobody is subscribed.
- `Subscribe`, `Apply` and `Changed` each carry a revision with the values read at it.
- A client takes values whose revision is newer than what it holds and ignores the rest. Each carries every value, so a missed `Changed` is made up by the next one or by an `Apply` reply.
- The daemon sends a write's signals before its reply. A client that handles messages in arrival order sees its own change first.

## Lifetime

- A subscription is the descriptor `Subscribe` returns, the write end of a pipe. It lasts while any copy of it is open, and the kernel tells the daemon when the last one closes, whether the client closed it, crashed or never received the reply. There is no call to end one.
- A new owner of `org.mechanix.MxConfig` means the daemon restarted and forgot every subscription. Clients subscribe again, and take the reply whatever its revision. It replaces every value and asset.
- A client skips a `Changed` it can't read, since the next one is whole. One that can't read an `AssetChanged` subscribes to that namespace again instead of guessing.

## Access

Every caller is an app, and its manifest says what it may reach.

- **Which app:** the daemon finds the caller's process with `GetConnectionUnixProcessID`. A Flatpak app is the `name` in its `/.flatpak-info`, whatever `app` says. A Snap, or a process it can't inspect, is no app. Any other caller is the `app` it passes, and no app if it is empty.
- **Its own app namespace**, the one named like the app, needs no manifest.
- **Every other namespace**, shared or another app's, needs a grant in the app's manifest, `/usr/share/mxconfig/manifests/<app>.toml`: `read` or `read-write`.
- **No app** reaches nothing.
- An unsandboxed app's `app` is taken on trust. This keeps honest apps to what they declare. It doesn't stop a hostile process, which could edit the files anyway.
- On the **system bus**, polkit also approves every write, as action `org.mechanix.mxconfig.write` with the namespace in its details. It is asked only once the write is known to be acceptable, so a refused write never prompts. The call may allow interactive authorization.

## Errors

| D-Bus error | When |
| --- | --- |
| `org.freedesktop.DBus.Error.AccessDenied` | Access denied, polkit refused, a key or asset is managed, or an asset write to a privileged namespace. |
| `org.freedesktop.DBus.Error.InvalidArgs` | Bad TOML, an unknown key, a value the schema rejects, a schema that doesn't parse or names another namespace, or an asset fd that isn't a regular file of at most 64 MiB. |
| `org.freedesktop.DBus.Error.UnknownObject` | The namespace is neither installed nor registered, or the other daemon serves it. |
| `org.mechanix.MxConfig.Error.VersionConflict` | Another schema version holds the namespace. |
| `org.freedesktop.DBus.Error.Failed` | Anything else, such as a file that can't be written. |

A refused `Apply` changes nothing.
