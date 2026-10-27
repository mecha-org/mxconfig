# Runtimes and transports

For people connecting mxconfig to an event loop or a D-Bus library. Apps don't need this.

- [Driving a source](#driving-a-source)
- [Handing events to the app](#handing-events-to-the-app)
- [Carrying the bus](#carrying-the-bus)
- [The adapters](#the-adapters)

## Driving a source

A `Source` never blocks, spawns threads or polls. A runtime moves data in and out of it with the methods of `mxconfig::runtime::Drive`, and takes the results with `outcomes()`:

| Call | What the runtime does |
| --- | --- |
| `inotify_fd()` | Watches this fd, with a read armed on it or a readiness source. |
| `feed_inotify(&bytes)` | Hands over each read. Only the namespaces it concerns are re-read. |
| `take_watch_dirs()`, `touched(path)`, `rescan()` | Used instead of the fd by a runtime with its own file watcher, like Dart's. |
| `reach(bus)`, `unreach(bus)` | Says it can, or can no longer, carry calls on a bus. Without a bus, app namespaces stay on files and shared ones on their defaults. |
| `close(bus)` | Lets go of a bus whose connection stays open. Handles that followed the daemon detach, ending their subscriptions. |
| `take_calls(bus)` | Takes the calls to send. |
| `deliver(bus, event)` | Hands back each reply, error and matched signal, in arrival order. |
| `outcomes()` | Takes what happened. |

Call `take_calls` and `outcomes` after feeding the source and before sleeping, since app code may have opened or written something meanwhile.

## Handing events to the app

Each outcome is about one namespace. For each client type the app requested in it, `outcome.event::<T>()` gives the `Event<T>` to raise.

- Each `request` is answered by one `Opened` or `OpenFailed`, carrying the request's ticket. Give it only to the client type that made that request, since two client types of one namespace can get different answers.
- Every other outcome goes to every client type of its namespace.
- `Diagnosed` is the same for every client type, so log it once per outcome.

## Carrying the bus

The library never opens a D-Bus connection. A runtime carries its calls on its own D-Bus library, through an adapter that converts values to and from `runtime::wire::Arg`. The adapter:

1. **Adds each of `wire::RULES`** as a signal match.
2. **Sends each `wire::Call`** to its destination, path, interface and member, and expects a reply. It allows interactive authorization if `call.interactive` is set.
3. **Delivers what comes back:**
   - a reply as `Event::Reply { id, args }`;
   - an error as `Event::Failed { id, name, message }`;
   - a matched signal as `Event::Signal`;
   - a call that couldn't be sent as `Event::unsent(id, message)`;
   - a call with no reply in time as `Event::timed_out(id)`. Allow 25 seconds, or 5 minutes for an interactive call, so a stuck daemon never hangs the app;
   - a value it couldn't convert as `Arg::Unreadable`.
4. **Hands over descriptors** as `Arg::Fd`. A subscription is one: the source holds it while handles need it, and closing it ends the subscription. An adapter that can't hand one over, like Dart's, keeps the reply's first descriptor itself when `call.keep` is set. It passes `Drive::kept(bus, id)` instead, and closes the ids `Drive::take_released(bus)` returns.

The source does the rest:

- It asks who owns the daemon's name when each bus is reached, and takes signals only from that owner.
- It subscribes again after a daemon restart or an unreadable `AssetChanged`.
- Each `Changed` carries every value, so the next one makes up for a missed one.

The messages themselves are specified in [`protocol.md`](protocol.md).

## The adapters

- **`mxconfig_dbus`**, a Dart package over `package:dbus`. It implements `Transport` from `package:mxconfig_dart/runtime.dart`, which apps don't import. `attach` hands it the source as a `Driven`, whose `transport` takes calls, replies and signals, and whose `drain()` handles the outcomes.
- **A zbus adapter** drives the daemon's tests, in `crates/mxconfig-daemon/tests/common/zbus_link.rs`. It's a short example for writing one in Rust.
