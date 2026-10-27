//! Tests for `Source` outcomes, on both backends.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::os::fd::OwnedFd;

use common::{ACCESS_DENIED, DAEMON, SCHEMA, answer, changed, detached, failed, fd, from};
use common::{members, open, opened, owner, raw_state, reach, remote, state, subscribed};
use mxconfig::host::Roots;
use mxconfig::host::protocol::{ASSET_CHANGED, CHANGED, INTERFACE};
use mxconfig::runtime::Drive;
use mxconfig::runtime::Event;
use mxconfig::runtime::wire::{Arg, Call};
use mxconfig::{Bus, Change, ErrorKind};
use mxconfig::{Detach, Edit, Error, FromSettings, Namespace, Origin, Outcome, Settings, Write};
use mxconfig::{Source, Ticket, Value};

const NS: &str = "org.mechanix.demo";

const WITH_ASSET: &str = r#"
namespace = "org.mechanix.demo"
version = 1

[assets.layout]
path = "layout.yaml"

[keys.level]
type = "integer"
default = 1
label = "Level"
description = "d"
"#;

/// A hand-written typed client, like a generated one.
struct Demo(Settings);

#[derive(Debug, PartialEq)]
enum DemoChange {
    Level(i64),
    AssetLayout,
}

impl FromSettings for Demo {
    fn from_settings(settings: Settings) -> Self {
        Demo(settings)
    }
}

impl Namespace for Demo {
    const NAMESPACE: &'static str = NS;
    const SCHEMA: &'static str = WITH_ASSET;
    type Change = DemoChange;

    fn change(change: &Change) -> Option<DemoChange> {
        match change.key.as_str() {
            "level" => Some(DemoChange::Level(change.value.as_ref()?.as_integer()?)),
            _ => None,
        }
    }

    fn asset_change(id: &str) -> Option<DemoChange> {
        (id == "layout").then_some(DemoChange::AssetLayout)
    }
}

/// `WITH_ASSET`, for an app that asks for the daemon only.
const REMOTE: &str = r#"
namespace = "org.mechanix.demo"
version = 1
backend = "daemon-only"

[assets.layout]
path = "layout.yaml"

[keys.level]
type = "integer"
default = 1
label = "Level"
description = "d"
"#;

/// `Demo`, opened through the daemon only.
struct Remote(Settings);

impl FromSettings for Remote {
    fn from_settings(settings: Settings) -> Self {
        Remote(settings)
    }
}

impl Namespace for Remote {
    const NAMESPACE: &'static str = NS;
    const SCHEMA: &'static str = REMOTE;
    type Change = DemoChange;

    fn change(change: &Change) -> Option<DemoChange> {
        Demo::change(change)
    }

    fn asset_change(id: &str) -> Option<DemoChange> {
        Demo::asset_change(id)
    }
}

/// A source with `schema` installed in a new tree.
fn source(tmp: &std::path::Path, schema: &str) -> (Source, Roots) {
    let roots = Roots::under(tmp);
    common::install(&roots, NS, schema);
    (Source::with_roots(roots.clone()), roots)
}

/// The result of write `ticket` among `outcomes`.
fn written(outcomes: &[Outcome], ticket: Ticket<Write>) -> Result<(), Error> {
    outcomes
        .iter()
        .find_map(|o| ticket.written(o))
        .expect("a Written outcome")
}

#[test]
fn a_local_open_and_write_complete_in_the_same_turn() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, _) = source(tmp.path(), WITH_ASSET);
    let demo = opened(&source, source.request::<Demo>()).unwrap();
    assert_eq!(demo.0.get("level"), Some(Value::Integer(1)));

    let write = demo.0.write(&[Edit::set("level", 5)]).unwrap();
    let outcomes = source.outcomes();
    // `Written` only reports success. The change arrives as `Changed`.
    written(&outcomes, write).unwrap();
    let typed: Vec<DemoChange> = outcomes
        .iter()
        .filter_map(Outcome::change::<Demo>)
        .collect();
    assert_eq!(typed, vec![DemoChange::Level(5)]);

    // A bad value is refused before anything is queued.
    assert!(demo.0.write(&[Edit::set("level", "high")]).is_err());
    assert!(source.outcomes().is_empty());
}

#[test]
fn file_and_asset_edits_arrive_as_outcomes() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, roots) = source(tmp.path(), WITH_ASSET);
    let _demo = opened(&source, source.request::<Demo>()).unwrap();

    let dir = roots.user_dir.as_ref().unwrap().join(NS);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("config.toml"), "level = 9\n").unwrap();
    fs::write(dir.join("layout.yaml"), "rows: []\n").unwrap();

    // A runtime with its own watcher reports the changed path.
    source.touched(&dir.join("config.toml"));
    let typed: Vec<DemoChange> = source
        .outcomes()
        .iter()
        .filter_map(Outcome::change::<Demo>)
        .collect();
    assert!(typed.contains(&DemoChange::Level(9)), "{typed:?}");
    assert!(typed.contains(&DemoChange::AssetLayout), "{typed:?}");

    // Nothing was read, so nothing is reported.
    source.feed_inotify(b"");
    assert!(source.outcomes().is_empty());
}

#[test]
fn opening_twice_shares_one_handle() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, _) = source(tmp.path(), WITH_ASSET);
    let first = opened(&source, source.request::<Demo>()).unwrap();
    let second = opened(&source, source.request::<Demo>()).unwrap();
    first.0.write(&[Edit::set("level", 3)]).unwrap();
    assert_eq!(second.0.get("level"), Some(Value::Integer(3)));
    // One handle, so one Changed per change.
    let changed = source
        .outcomes()
        .iter()
        .filter(|o| matches!(o, Outcome::Changed(_)))
        .count();
    assert_eq!(changed, 1);
}

/// The keys of the problems `outcomes` report as `Diagnosed`.
fn diagnosed(outcomes: &[Outcome]) -> Vec<Option<String>> {
    let keys = |o: &Outcome| match o {
        Outcome::Diagnosed { diagnostics, .. } => diagnostics
            .iter()
            .map(|d| d.key().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    };
    outcomes.iter().flat_map(keys).collect()
}

/// An open reports its problems once, after `Opened`.
#[test]
fn an_open_reports_the_problems_of_its_files_once() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, roots) = source(tmp.path(), SCHEMA);
    let user = roots.user_dir.as_ref().unwrap();
    common::put(&user.join(NS).join("config.toml"), "verbose = 3\n");
    let ticket = source.request_namespace(NS);
    let outcomes = source.outcomes();
    assert_eq!(outcomes.len(), 2, "{outcomes:?}");
    assert!(ticket.opened(&outcomes[0]).unwrap().is_ok());
    assert_eq!(diagnosed(&outcomes), [Some("verbose".to_string())]);

    // Opening it again shares the handle, and reports nothing new.
    let again = source.request_namespace(NS);
    let outcomes = source.outcomes();
    assert!(again.opened(&outcomes[0]).unwrap().is_ok());
    assert!(diagnosed(&outcomes).is_empty(), "{outcomes:?}");
}

/// A reread reports only problems that are new.
#[test]
fn a_reread_reports_only_new_problems() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, roots) = source(tmp.path(), SCHEMA);
    let user = roots.user_dir.as_ref().unwrap();
    let file = common::put(&user.join(NS).join("config.toml"), "verbose = 3\n");
    let _settings = open(&source, NS).unwrap();

    // The same problems again report nothing.
    fs::write(&file, "verbose = 3\n").unwrap();
    source.rescan();
    let outcomes = source.outcomes();
    assert!(diagnosed(&outcomes).is_empty(), "{outcomes:?}");

    // An edit that adds a problem reports only that one.
    fs::write(&file, "verbose = 3\nnickname = 4\n").unwrap();
    source.rescan();
    assert_eq!(
        diagnosed(&source.outcomes()),
        [Some("nickname".to_string())]
    );
}

/// Answer `source`'s session bus calls like the daemon.
fn serve(source: &Source, daemon: &mut BTreeMap<String, Value>, revision: &mut u64) -> Vec<Call> {
    let calls = source.take_calls(Bus::Session);
    for call in &calls {
        let args = match call.member {
            "Subscribe" => subscribed(SCHEMA, *revision, daemon, &[]),
            "Apply" => {
                *revision += 1;
                for (key, value) in edits(call) {
                    match value {
                        Some(value) => daemon.insert(key, value),
                        None => daemon.remove(&key),
                    };
                }
                // Like mxconfigd: send the state's signal before the reply.
                source.deliver(Bus::Session, changed(NS, *revision, daemon));
                state(*revision, daemon)
            }
            _ => Vec::new(),
        };
        source.deliver(Bus::Session, answer(call.id, args));
    }
    calls
}

/// Open `NS` through the fake daemon holding `daemon` at `revision`.
fn serve_open(
    source: &Source,
    daemon: &mut BTreeMap<String, Value>,
    revision: &mut u64,
) -> Settings {
    reach(source);
    let ticket = source.request_namespace(NS);
    serve(source, daemon, revision);
    opened(source, ticket).unwrap()
}

/// The schema a `Subscribe` call sends, empty if none.
fn schema(call: &Call) -> &str {
    match &*call.args {
        [_, Arg::Str(schema), _] => schema,
        _ => panic!("{call:?}"),
    }
}

/// The edits an `Apply` call sends: each key and its value, `None` when unset.
fn edits(call: &Call) -> Vec<(String, Option<Value>)> {
    let Arg::Array(_, entries) = &call.args[1] else {
        panic!("{call:?}")
    };
    let edit = |entry: &Arg| {
        let Arg::Struct(fields) = entry else {
            panic!("{entry:?}")
        };
        let [Arg::Str(key), Arg::Bool(set), Arg::Str(text)] = &**fields else {
            panic!("{entry:?}")
        };
        let value = set.then(|| mxconfig::value::from_toml_str(text).unwrap());
        (key.clone(), value)
    };
    entries.iter().map(edit).collect()
}

/// A subscription descriptor as the daemon passes one, and the end the daemon watches.
fn subscription() -> (OwnedFd, Arg) {
    use nix::fcntl::OFlag;
    let (watched, kept) = nix::unistd::pipe2(OFlag::O_CLOEXEC | OFlag::O_NONBLOCK).unwrap();
    (watched, Arg::Fd(std::sync::Arc::new(kept)))
}

/// Whether the client has ended the subscription the daemon watches through `watched`.
fn ended(watched: &OwnedFd) -> bool {
    matches!(nix::unistd::read(watched, &mut [0u8; 1]), Ok(0))
}

/// Answer `source`'s first `Subscribe` with `subscription`, and open it.
fn subscribe_with(source: &Source, subscription: Arg) -> Settings {
    reach(source);
    let ticket = source.request_namespace(NS);
    let id = source.take_calls(Bus::Session)[0].id;
    let mut reply = subscribed(SCHEMA, 0, &BTreeMap::new(), &[]);
    reply[0] = subscription;
    source.deliver(Bus::Session, answer(id, reply));
    opened(source, ticket).unwrap()
}

/// One value, as a daemon state holds it.
fn one(key: &str, value: Value) -> BTreeMap<String, Value> {
    BTreeMap::from([(key.to_string(), value)])
}

/// An unreadable `Changed` is skipped, and the next whole one lands.
#[test]
fn an_unreadable_change_is_skipped_instead_of_unsetting() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, _) = source(tmp.path(), &remote(SCHEMA));
    let mut daemon = one("colors.corner_radius", Value::Integer(4));
    let settings = serve_open(&source, &mut daemon, &mut 0);

    let unreadable = raw_state(1, vec![("colors.corner_radius".into(), "9 =".into())]);
    let args = [vec![Arg::Str(NS.into())], unreadable].concat();
    source.deliver(Bus::Session, from(DAEMON, INTERFACE, CHANGED, args));
    assert_eq!(
        settings.get("colors.corner_radius"),
        Some(Value::Integer(4))
    );
    assert!(source.take_calls(Bus::Session).is_empty(), "no resubscribe");
    let next = one("colors.corner_radius", Value::Integer(9));
    source.deliver(Bus::Session, changed(NS, 2, &next));
    assert_eq!(
        settings.get("colors.corner_radius"),
        Some(Value::Integer(9))
    );
}

#[test]
fn a_bus_the_runtime_lets_go_of_fails_what_waits_on_it() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, roots) = source(tmp.path(), &remote(SCHEMA));
    let other = remote(&SCHEMA.replace(NS, "org.mechanix.other"));
    common::install(&roots, "org.mechanix.other", &other);
    reach(&source);
    let in_flight = source.request_namespace(NS);
    source.take_calls(Bus::Session);
    let queued = source.request_namespace("org.mechanix.other");

    source.unreach(Bus::Session);
    let outcomes = source.outcomes();
    let failed = |ticket: Ticket<Settings>| {
        let result = outcomes.iter().find_map(|o| ticket.opened(o));
        matches!(result, Some(Err(e)) if e.kind() == ErrorKind::Unreachable(Bus::Session))
    };
    assert!(failed(in_flight), "an answer that will never come");
    assert!(failed(queued), "a request no one will send");
    assert!(source.take_calls(Bus::Session).is_empty());
    let again = source.request_namespace(NS);
    assert!(
        matches!(opened(&source, again), Err(e) if matches!(e.kind(), ErrorKind::Unreachable(_)))
    );
}

#[test]
fn only_the_daemon_is_heard_and_only_a_new_owner_is_a_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, _) = source(tmp.path(), &remote(SCHEMA));
    let settings = serve_open(&source, &mut BTreeMap::new(), &mut 0);

    // A `Changed` from another peer is ignored.
    let mut spoof = changed(NS, 1, &one("colors.corner_radius", Value::Integer(1)));
    if let Event::Signal { sender, .. } = &mut spoof {
        *sender = ":1.66".into();
    }
    source.deliver(Bus::Session, spoof);
    assert_eq!(settings.get("colors.corner_radius"), None);

    // The owner the replies came from is not a restart.
    source.deliver(Bus::Session, owner(DAEMON));
    assert!(source.take_calls(Bus::Session).is_empty());

    // The owner leaving and a new one appearing is a restart, so resubscribe.
    source.deliver(Bus::Session, owner(""));
    source.deliver(Bus::Session, owner(DAEMON));
    let asked = source.take_calls(Bus::Session);
    assert!(members(&asked) == ["Subscribe"], "{asked:?}");
}

#[test]
fn a_daemon_open_completes_only_once_answered() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, _) = source(tmp.path(), &remote(SCHEMA));
    let mut daemon = one("colors.accent", Value::String("#123456".into()));
    reach(&source);
    let ticket = source.request_namespace(NS);
    assert!(
        source.outcomes().is_empty(),
        "nothing before the daemon answers"
    );
    // An untyped open subscribes in one call, carrying no schema of its own.
    let asked = serve(&source, &mut daemon, &mut 0);
    assert_eq!(members(&asked), ["Subscribe"]);
    assert_eq!(schema(&asked[0]), "");
    let settings = opened(&source, ticket).unwrap();
    assert!(settings.from_daemon());
    assert_eq!(
        settings.get("colors.accent"),
        Some(Value::String("#123456".into()))
    );
    assert_eq!(
        settings.resolved().origin("colors.accent"),
        Some(&Origin::Daemon)
    );
}

#[test]
fn a_daemon_write_lands_only_once_answered() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, _) = source(tmp.path(), &remote(SCHEMA));
    let (mut daemon, mut revision) = (BTreeMap::new(), 0);
    let settings = serve_open(&source, &mut daemon, &mut revision);
    let write = settings
        .write(&[Edit::set("colors.corner_radius", 12)])
        .unwrap();
    assert!(source.outcomes().is_empty());
    assert_eq!(
        settings.get("colors.corner_radius"),
        None,
        "not yet applied"
    );
    serve(&source, &mut daemon, &mut revision);
    let outcomes = source.outcomes();
    written(&outcomes, write).unwrap();
    let changed = outcomes
        .iter()
        .filter(|o| matches!(o, Outcome::Changed(_)))
        .count();
    assert_eq!(changed, 1);
    assert_eq!(
        settings.get("colors.corner_radius"),
        Some(Value::Integer(12))
    );
}

/// A refusal is an error and changes nothing.
#[test]
fn a_refused_daemon_write_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, _) = source(tmp.path(), &remote(SCHEMA));
    let settings = open_with_daemon(&source);
    let write = settings
        .write(&[Edit::set("colors.corner_radius", 20)])
        .unwrap();
    let id = source.take_calls(Bus::Session)[0].id;
    source.deliver(Bus::Session, failed(id, ACCESS_DENIED));
    let refused = written(&source.outcomes(), write).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Denied);
    assert_eq!(
        settings.get("colors.corner_radius"),
        Some(Value::Integer(1))
    );
}

/// A reply ahead of what is held means a lost signal. Its values heal that.
#[test]
fn a_write_reply_ahead_of_the_held_state_heals_a_lost_signal() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, _) = source(tmp.path(), &remote(SCHEMA));
    let (mut daemon, mut revision) = (BTreeMap::new(), 0);
    let settings = serve_open(&source, &mut daemon, &mut revision);
    let write = settings.write(&[Edit::set("verbose", true)]).unwrap();
    let id = source.take_calls(Bus::Session)[0].id;
    daemon.insert("verbose".into(), Value::Boolean(true));
    revision += 2;
    source.deliver(Bus::Session, answer(id, state(revision, &daemon)));
    assert!(source.take_calls(Bus::Session).is_empty(), "no resubscribe");
    assert_eq!(settings.revision(), revision);
    let outcomes = source.outcomes();
    assert!(
        outcomes
            .iter()
            .any(|o| matches!(o, Outcome::Changed(c) if c.key == "verbose"))
    );
    written(&outcomes, write).unwrap();
    assert_eq!(settings.get("verbose"), Some(Value::Boolean(true)));
}

#[test]
fn a_newer_state_replaces_the_values_and_a_restart_replaces_any() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, _) = source(tmp.path(), &remote(SCHEMA));
    let settings = serve_open(&source, &mut BTreeMap::new(), &mut 0);

    let accent = |text: &str| Value::String(text.into());
    let first = one("colors.accent", accent("#010203"));
    source.deliver(Bus::Session, changed(NS, 1, &first));
    // A repeated or older state changes nothing.
    source.deliver(Bus::Session, changed(NS, 1, &first));
    let older = one("colors.accent", accent("#ffffff"));
    source.deliver(Bus::Session, changed(NS, 0, &older));
    assert_eq!(source.outcomes().len(), 1);
    assert_eq!(settings.get("colors.accent"), Some(accent("#010203")));

    // Revision 5 after 1 skips missed states, as each one is whole.
    let fifth = one("verbose", Value::Boolean(true));
    source.deliver(Bus::Session, changed(NS, 5, &fifth));
    assert!(source.take_calls(Bus::Session).is_empty(), "no resubscribe");
    assert_eq!(settings.get("verbose"), Some(Value::Boolean(true)));
    assert_eq!(
        settings.get("colors.accent"),
        None,
        "the unset key is unset"
    );
    assert_eq!(settings.revision(), 5);
    let outcomes = source.outcomes();
    assert_eq!(outcomes.len(), 2, "one set, one unset: {outcomes:?}");

    // After a restart, subscribe again.
    source.deliver(Bus::Session, owner(""));
    source.deliver(Bus::Session, owner(":1.99"));
    let asked = source.take_calls(Bus::Session);
    assert_eq!(members(&asked), ["Subscribe"]);
    // The new daemon starts again at 0, and its state still replaces.
    let fresh = one("colors.accent", accent("#0a0b0c"));
    let reply = subscribed(SCHEMA, 0, &fresh, &[]);
    source.deliver(Bus::Session, answer(asked[0].id, reply));
    assert_eq!(settings.revision(), 0);
    assert_eq!(settings.get("colors.accent"), Some(accent("#0a0b0c")));
    assert_eq!(settings.get("verbose"), None);
    let mut next = fresh.clone();
    next.insert("verbose".into(), Value::Boolean(false));
    let event = changed(NS, 1, &next);
    let Event::Signal { args, .. } = event else {
        unreachable!()
    };
    source.deliver(Bus::Session, from(":1.99", INTERFACE, CHANGED, args));
    assert_eq!(settings.get("verbose"), Some(Value::Boolean(false)));
}

/// A `Changed` that overtakes its `Subscribe` reply still lands, if newer.
#[test]
fn a_change_heard_before_the_subscribe_reply_is_kept() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, _) = source(tmp.path(), &remote(SCHEMA));
    reach(&source);
    let ticket = source.request_namespace(NS);
    let id = source.take_calls(Bus::Session)[0].id;
    let level = |n| one("colors.corner_radius", Value::Integer(n));
    source.deliver(Bus::Session, changed(NS, 3, &level(3)));
    source.deliver(Bus::Session, changed(NS, 2, &level(2)));
    // A stranger's is dropped once the reply names the daemon.
    let Event::Signal { args, .. } = changed(NS, 9, &level(9)) else {
        unreachable!()
    };
    source.deliver(Bus::Session, from(":1.666", INTERFACE, CHANGED, args));
    source.deliver(
        Bus::Session,
        answer(id, subscribed(SCHEMA, 1, &level(1), &[])),
    );
    let settings = opened(&source, ticket).unwrap();
    assert_eq!(settings.revision(), 3);
    assert_eq!(
        settings.get("colors.corner_radius"),
        Some(Value::Integer(3))
    );

    // A resubscribe keeps the newest too.
    source.deliver(Bus::Session, owner(""));
    source.deliver(Bus::Session, owner(DAEMON));
    let id = subscribes(&source)[0];
    source.deliver(Bus::Session, changed(NS, 2, &level(5)));
    source.deliver(
        Bus::Session,
        answer(id, subscribed(SCHEMA, 1, &level(4), &[])),
    );
    assert_eq!(
        settings.get("colors.corner_radius"),
        Some(Value::Integer(5))
    );
}

/// A subscription lasts exactly as long as its handles, with nothing to send.
#[test]
fn the_last_handle_going_away_ends_the_subscription() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, _) = source(tmp.path(), &remote(SCHEMA));
    let (watched, sent) = subscription();
    let settings = subscribe_with(&source, sent);
    let other = settings.clone();
    drop(settings);
    assert!(!ended(&watched), "a handle remains");
    drop(other);
    assert!(ended(&watched));
    assert!(
        source.take_calls(Bus::Session).is_empty(),
        "nothing to send"
    );
}

/// A reply the client refuses ends its subscription at once.
#[test]
fn a_refused_reply_ends_its_subscription() {
    let tmp = tempfile::tempdir().unwrap();
    let source = Source::with_roots(Roots::under(tmp.path()));
    reach(&source);
    let ticket = source.request::<Remote>();
    let id = source.take_calls(Bus::Session)[0].id;
    let (watched, sent) = subscription();
    let mut reply = subscribed(&with_asset_v2(), 0, &BTreeMap::new(), &[]);
    reply[0] = sent;
    source.deliver(Bus::Session, answer(id, reply));
    let refused = opened(&source, ticket).err().expect("another version");
    assert_eq!(refused.kind(), ErrorKind::VersionConflict);
    assert!(ended(&watched));
}

/// A transport that can't hand descriptors over is told when to close one.
#[test]
fn a_kept_subscription_is_released_through_the_transport() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, _) = source(tmp.path(), &remote(SCHEMA));
    let kept = source.kept(Bus::Session, 7);
    let settings = subscribe_with(&source, kept);
    assert_eq!(source.take_released(Bus::Session), Vec::<u64>::new());
    drop(settings);
    assert_eq!(source.take_released(Bus::Session), [7]);
    assert_eq!(source.take_released(Bus::System), Vec::<u64>::new());
}

/// Closing the bus detaches what follows the daemon, and ends its subscriptions.
#[test]
fn closing_the_bus_detaches_and_ends_the_subscriptions() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, roots) = source(tmp.path(), &remote(SCHEMA));
    let other = remote(&SCHEMA.replace(NS, "org.mechanix.other"));
    common::install(&roots, "org.mechanix.other", &other);
    let (watched, sent) = subscription();
    let settings = subscribe_with(&source, sent);

    // A `Subscribe` on the wire, a write on the wire, and a write never sent.
    let in_flight = source.request_namespace("org.mechanix.other");
    let write = settings
        .write(&[Edit::set("colors.corner_radius", 5)])
        .unwrap();
    let calls = source.take_calls(Bus::Session);
    assert_eq!(members(&calls), ["Subscribe", "Apply"]);
    let unsent = settings
        .write(&[Edit::set("colors.corner_radius", 6)])
        .unwrap();

    source.close(Bus::Session);
    assert!(ended(&watched), "the handle let go of its subscription");
    assert!(
        source.take_calls(Bus::Session).is_empty(),
        "nothing is sent"
    );
    let outcomes = source.outcomes();
    let closed = |o: &Outcome| matches!(o, Outcome::Detached { namespace, cause: Detach::Closed } if namespace == NS);
    assert!(outcomes.iter().any(closed), "{outcomes:?}");
    let kind = |ticket: Ticket<Write>| written(&outcomes, ticket).unwrap_err().kind();
    assert_eq!(kind(write), ErrorKind::Unconfirmed, "it may have landed");
    assert_eq!(
        kind(unsent),
        ErrorKind::Unreachable(Bus::Session),
        "it never left"
    );
    assert!(
        outcomes
            .iter()
            .find_map(|o| in_flight.opened(o))
            .unwrap()
            .is_err()
    );

    // The in-flight `Subscribe`'s reply arrives after all: its subscription ends at once.
    let (late, sent) = subscription();
    let mut reply = subscribed(SCHEMA, 0, &BTreeMap::new(), &[]);
    reply[0] = sent;
    source.deliver(Bus::Session, answer(calls[0].id, reply));
    assert!(ended(&late));

    // It keeps its values but refuses writes and reopening.
    assert_eq!(settings.get("colors.corner_radius"), None);
    let e = settings
        .write(&[Edit::set("colors.corner_radius", 7)])
        .unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Detached);
    assert!(e.to_string().contains("transport was closed"), "{e}");
    reach(&source);
    let again = source.request_namespace(NS);
    assert_eq!(
        opened(&source, again).unwrap_err().kind(),
        ErrorKind::Detached
    );
    drop(settings);
    let _again = source.request_namespace(NS);
    assert_eq!(members(&source.take_calls(Bus::Session)), ["Subscribe"]);
}

/// A write that times out may have landed. A read that times out did not.
#[test]
fn a_write_that_times_out_is_unconfirmed() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, roots) = source(tmp.path(), &remote(SCHEMA));
    let other = remote(&SCHEMA.replace(NS, "org.mechanix.other"));
    common::install(&roots, "org.mechanix.other", &other);
    let (_watched, sent) = subscription();
    let settings = subscribe_with(&source, sent);

    let in_flight = source.request_namespace("org.mechanix.other");
    let write = settings
        .write(&[Edit::set("colors.corner_radius", 5)])
        .unwrap();
    let calls = source.take_calls(Bus::Session);
    assert_eq!(members(&calls), ["Subscribe", "Apply"]);
    for call in &calls {
        source.deliver(Bus::Session, Event::timed_out(call.id));
    }

    let outcomes = source.outcomes();
    let refused = written(&outcomes, write).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Unconfirmed);
    let opened = outcomes.iter().find_map(|o| in_flight.opened(o)).unwrap();
    assert_eq!(
        opened.unwrap_err().kind(),
        ErrorKind::Unreachable(Bus::Session)
    );
}

#[test]
fn the_watch_set_is_handed_out_only_when_it_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, _) = source(tmp.path(), SCHEMA);
    let demo: Demo = opened(&source, source.request()).unwrap();
    assert!(source.take_watch_dirs().is_some(), "the first time");
    assert!(source.take_watch_dirs().is_none(), "nothing changed since");
    source.rescan();
    assert!(
        source.take_watch_dirs().is_none(),
        "a rescan alone changes nothing"
    );
    let other = source.open_schema(
        mxconfig::Schema::parse(&SCHEMA.replace("org.mechanix.demo", "org.mechanix.other"))
            .unwrap(),
    );
    assert!(
        source.take_watch_dirs().is_some(),
        "a new namespace needs watching"
    );
    drop((demo, other));
    let left = source
        .take_watch_dirs()
        .expect("the watches went with the handles");
    assert!(left.is_empty(), "{left:?}");
}

/// A user file is copied when it parses, and that copy is used once it breaks.
#[test]
fn a_user_file_read_whole_becomes_the_last_good_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, roots) = source(tmp.path(), SCHEMA);
    let user = roots.user_dir.as_ref().unwrap();
    let good = "[colors]\naccent = \"#ff0000\"\n";
    let file = common::put(&user.join(NS).join("config.toml"), good);
    let settings = open(&source, NS).unwrap();
    let lastgood = file.with_file_name("config.toml.lastgood");
    assert_eq!(fs::read_to_string(&lastgood).unwrap(), good);

    fs::write(&file, "[colors\n").unwrap();
    source.rescan();
    let red = Some(Value::String("#ff0000".into()));
    assert_eq!(
        settings.get("colors.accent"),
        red,
        "the last good copy stands in"
    );
    let broken = file.with_file_name("config.toml.broken");
    assert_eq!(fs::read_to_string(&broken).unwrap(), "[colors\n");
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "[colors\n",
        "the file is left alone"
    );
    assert_eq!(fs::read_to_string(&lastgood).unwrap(), good);
}

/// Only user files get copies.
#[test]
fn files_below_the_user_layer_get_no_copies() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, roots) = source(tmp.path(), SCHEMA);
    let admin = roots.admin_dir.as_ref().unwrap();
    let file = common::put(&admin.join(NS).join("config.toml"), "[colors\n");
    open(&source, NS).unwrap();
    let names: Vec<_> = fs::read_dir(file.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["config.toml"]);
}

/// A client opens with its embedded schema when none is installed.
#[test]
fn a_typed_open_without_an_installed_schema_uses_its_own() {
    let tmp = tempfile::tempdir().unwrap();
    let source = Source::with_roots(Roots::under(tmp.path()));
    let demo = opened(&source, source.request::<Demo>()).unwrap();
    assert_eq!(demo.0.get("level"), Some(Value::Integer(1)));
}

/// A daemon open sends the embedded schema.
#[test]
fn a_daemon_open_registers_the_schema_the_client_carries() {
    let tmp = tempfile::tempdir().unwrap();
    let source = Source::with_roots(Roots::under(tmp.path()));
    reach(&source);
    let _ticket = source.request::<Remote>();
    let asked = source.take_calls(Bus::Session);
    assert_eq!(schema(&asked[0]), REMOTE);
}

/// `AssetChanged` for the `layout` asset, with its file at `path` if set.
fn asset_changed(sender: &str, path: Option<&std::path::Path>) -> Event {
    let file =
        |path: &std::path::Path| Arg::Struct(vec![Arg::Str(path.display().to_string()), fd(path)]);
    let file = Arg::Array("(sh)".into(), path.map(file).into_iter().collect());
    let args = vec![Arg::Str(NS.into()), Arg::Str("layout".into()), file];
    from(sender, INTERFACE, ASSET_CHANGED, args)
}

/// A daemon asset reads from the passed fd and follows `AssetChanged`.
#[test]
fn a_daemon_served_asset_reads_its_fd_and_follows_the_daemon() {
    let tmp = tempfile::tempdir().unwrap();
    let source = Source::with_roots(Roots::under(tmp.path()));
    reach(&source);
    let first = tmp.path().join("first.yaml");
    fs::write(&first, "one").unwrap();

    let ticket = source.request::<Remote>();
    let id = source.take_calls(Bus::Session)[0].id;
    let reply = subscribed(WITH_ASSET, 1, &BTreeMap::new(), &[("layout", &first)]);
    source.deliver(Bus::Session, answer(id, reply));
    let demo = opened(&source, ticket).unwrap();
    let asset = demo.0.asset("layout").expect("served");
    assert_eq!(asset.origin, Origin::Daemon);
    assert_eq!(asset.text().unwrap(), "one");
    // The fd still reads after the path is gone.
    fs::remove_file(&first).unwrap();
    assert_eq!(asset.text().unwrap(), "one");

    let second = tmp.path().join("second.yaml");
    fs::write(&second, "two").unwrap();
    source.deliver(Bus::Session, asset_changed(":1.66", Some(&second)));
    assert!(source.outcomes().is_empty(), "only the daemon is heard");
    source.deliver(Bus::Session, asset_changed(DAEMON, Some(&second)));
    let changes: Vec<_> = source
        .outcomes()
        .iter()
        .filter_map(mxconfig::Outcome::change::<Demo>)
        .collect();
    assert_eq!(changes, [DemoChange::AssetLayout]);
    assert_eq!(demo.0.asset("layout").unwrap().text().unwrap(), "two");

    source.deliver(Bus::Session, asset_changed(DAEMON, None));
    assert_eq!(source.outcomes().len(), 1);
    assert!(demo.0.asset("layout").is_none(), "nothing sets it now");
}

/// A resync reports every daemon asset as changed.
#[test]
fn a_resync_reports_every_served_asset() {
    let tmp = tempfile::tempdir().unwrap();
    let source = Source::with_roots(Roots::under(tmp.path()));
    reach(&source);
    let file = tmp.path().join("layout.yaml");
    fs::write(&file, "one").unwrap();
    let ticket = source.request::<Remote>();
    let id = source.take_calls(Bus::Session)[0].id;
    let reply = subscribed(WITH_ASSET, 1, &BTreeMap::new(), &[("layout", &file)]);
    source.deliver(Bus::Session, answer(id, reply));
    let demo = opened(&source, ticket).unwrap();

    // After a daemon restart, the client subscribes again and gets new files.
    source.deliver(Bus::Session, owner(""));
    source.deliver(Bus::Session, owner(DAEMON));
    let id = source.take_calls(Bus::Session)[0].id;
    fs::write(&file, "two").unwrap();
    let reply = subscribed(WITH_ASSET, 1, &BTreeMap::new(), &[("layout", &file)]);
    source.deliver(Bus::Session, answer(id, reply));
    let changes: Vec<_> = source
        .outcomes()
        .iter()
        .filter_map(mxconfig::Outcome::change::<Demo>)
        .collect();
    assert_eq!(changes, [DemoChange::AssetLayout]);
    assert_eq!(demo.0.asset("layout").unwrap().text().unwrap(), "two");
}

/// Open `Remote` through the fake daemon, with no assets.
fn open_remote(source: &Source) -> Remote {
    reach(source);
    let ticket = source.request::<Remote>();
    let id = source.take_calls(Bus::Session)[0].id;
    let reply = subscribed(WITH_ASSET, 1, &BTreeMap::new(), &[]);
    source.deliver(Bus::Session, answer(id, reply));
    opened(source, ticket).unwrap()
}

/// The `layout` write `source` queued: its call id and the file's bytes.
fn asset_write(source: &Source) -> (u64, Option<Vec<u8>>) {
    let call = source.take_calls(Bus::Session).remove(0);
    let bytes = match (call.member, (&*call.args)) {
        ("WriteAsset", [Arg::Fd(fd), _, Arg::Str(asset), _]) if asset == "layout" => {
            let mut bytes = Vec::new();
            let mut file = fs::File::from(fd.try_clone().unwrap());
            file.read_to_end(&mut bytes).unwrap();
            Some(bytes)
        }
        ("ClearAsset", [_, Arg::Str(asset), _]) if asset == "layout" => None,
        _ => panic!("{call:?}"),
    };
    (call.id, bytes)
}

/// Daemon asset writes send a file. A clear sends none.
#[test]
fn a_daemon_asset_write_sends_a_file_and_lands_once_answered() {
    let tmp = tempfile::tempdir().unwrap();
    let source = Source::with_roots(Roots::under(tmp.path()));
    let demo = open_remote(&source);

    let ticket = demo.0.write_asset("layout", b"bytes").unwrap();
    let (id, file) = asset_write(&source);
    assert_eq!(file.unwrap(), b"bytes");
    assert!(source.outcomes().is_empty(), "nothing before the answer");
    source.deliver(Bus::Session, answer(id, Vec::new()));
    written(&source.outcomes(), ticket).unwrap();

    let picked = tmp.path().join("picked.yaml");
    fs::write(&picked, "from a portal").unwrap();
    let fd = std::os::fd::OwnedFd::from(fs::File::open(&picked).unwrap());
    demo.0.write_asset_from("layout", fd).unwrap();
    let (_, file) = asset_write(&source);
    assert_eq!(file.unwrap(), b"from a portal");

    demo.0.clear_asset("layout").unwrap();
    let (id, file) = asset_write(&source);
    assert!(file.is_none(), "a clear sends no file");
    source.deliver(Bus::Session, failed(id, ACCESS_DENIED));
    let failed = source.outcomes().into_iter().find_map(|o| match o {
        Outcome::WriteFailed {
            namespace, error, ..
        } => Some((namespace, error)),
        _ => None,
    });
    let (namespace, error) = failed.expect("answered");
    assert_eq!(
        namespace, "org.mechanix.demo",
        "a write names its namespace"
    );
    assert_eq!(error.kind(), ErrorKind::Denied);
}

/// A local write from a file copies it into the user layer.
#[test]
fn a_local_asset_write_from_a_file_copies_it() {
    let tmp = tempfile::tempdir().unwrap();
    let source = Source::with_roots(Roots::under(tmp.path()));
    let demo = opened(&source, source.request::<Demo>()).unwrap();
    let picked = tmp.path().join("picked.yaml");
    fs::write(&picked, "chosen").unwrap();
    let fd = std::os::fd::OwnedFd::from(fs::File::open(&picked).unwrap());
    demo.0.write_asset_from("layout", fd).unwrap();
    fs::remove_file(&picked).unwrap();
    let asset = demo.0.asset("layout").expect("the user's copy");
    assert_eq!(asset.text().unwrap(), "chosen");
    assert!(matches!(
        asset.origin,
        Origin::File {
            kind: mxconfig::LayerKind::User,
            ..
        }
    ));
}

/// Open `NS` through the fake daemon, with `colors.corner_radius` at 1.
fn open_with_daemon(source: &Source) -> Settings {
    let mut daemon = one("colors.corner_radius", Value::Integer(1));
    serve_open(source, &mut daemon, &mut 1)
}

/// The ids of the `Subscribe` calls `source` queued.
fn subscribes(source: &Source) -> Vec<u64> {
    let calls = source.take_calls(Bus::Session).into_iter();
    let ids = calls.filter(|call| call.member == "Subscribe");
    ids.map(|call| call.id).collect()
}

/// An `AssetChanged` whose fd the transport could not convert.
fn unreadable(namespace: Option<&str>) -> Event {
    let args = match namespace {
        Some(namespace) => vec![Arg::Str(namespace.into()), Arg::Unreadable],
        None => Vec::new(),
    };
    from(DAEMON, INTERFACE, ASSET_CHANGED, args)
}

/// A failed resubscribe keeps the old values and adds a diagnostic.
#[test]
fn a_failed_resubscribe_keeps_the_values_and_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, _) = source(tmp.path(), &remote(SCHEMA));
    let settings = open_with_daemon(&source);
    source.deliver(Bus::Session, unreadable(Some(NS)));
    let ids = subscribes(&source);
    assert_eq!(ids.len(), 1);
    source.deliver(Bus::Session, failed(ids[0], ACCESS_DENIED));
    assert_eq!(
        settings.get("colors.corner_radius"),
        Some(Value::Integer(1))
    );
    let stale = settings
        .diagnostics()
        .into_iter()
        .any(|d| d.message().contains("stale"));
    assert!(stale, "{:?}", settings.diagnostics());
}

/// Bad signals while a resubscribe is in flight join it.
#[test]
fn one_resubscribe_at_a_time_per_namespace() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, _) = source(tmp.path(), &remote(SCHEMA));
    let _settings = open_with_daemon(&source);
    source.deliver(Bus::Session, unreadable(Some(NS)));
    source.deliver(Bus::Session, unreadable(Some(NS)));
    assert_eq!(subscribes(&source).len(), 1);
}

/// A restart replaces a resubscribe sent to the old daemon, whose reply is then ignored.
#[test]
fn a_restart_resends_a_resubscribe_in_flight() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, _) = source(tmp.path(), &remote(SCHEMA));
    let settings = open_with_daemon(&source);
    source.deliver(Bus::Session, unreadable(Some(NS)));
    let old = subscribes(&source);
    source.deliver(Bus::Session, owner(""));
    source.deliver(Bus::Session, owner(DAEMON));
    let new = subscribes(&source);
    assert_eq!(new.len(), 1, "one resubscribe to the new daemon");
    assert_ne!(new, old);
    let stale = one("colors.corner_radius", Value::Integer(99));
    source.deliver(
        Bus::Session,
        answer(old[0], subscribed(SCHEMA, 9, &stale, &[])),
    );
    assert_eq!(
        settings.get("colors.corner_radius"),
        Some(Value::Integer(1))
    );
}

/// An unreadable signal resubscribes its namespace if known, else every one.
#[test]
fn an_unreadable_signal_resubscribes_only_what_it_names() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, _) = source(tmp.path(), &remote(SCHEMA));
    let _settings = open_with_daemon(&source);
    source.deliver(Bus::Session, unreadable(Some("org.mechanix.other")));
    assert!(
        subscribes(&source).is_empty(),
        "another namespace is not open"
    );
    source.deliver(Bus::Session, unreadable(Some(NS)));
    let named = subscribes(&source);
    assert_eq!(named.len(), 1);
    source.deliver(
        Bus::Session,
        failed(named[0], "org.freedesktop.DBus.Error.Failed"),
    );
    source.deliver(Bus::Session, unreadable(None));
    assert_eq!(subscribes(&source).len(), 1, "every open namespace");
}

/// `SCHEMA`, for an app that prefers the daemon.
fn prefers_daemon() -> String {
    SCHEMA.replace("version = 2", "version = 2\nbackend = \"daemon\"")
}

/// With no schema here, an open falls back to the daemon and says so.
#[test]
fn an_open_falls_back_to_the_daemon_and_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let source = Source::with_roots(Roots::under(tmp.path()));
    reach(&source);
    let ticket = source.request_namespace(NS);
    assert!(source.outcomes().is_empty(), "waiting on the daemon");
    serve(&source, &mut BTreeMap::new(), &mut 0);
    let settings = opened(&source, ticket).unwrap();
    assert!(settings.from_daemon());
    let notes = settings.diagnostics();
    let said = |d: &mxconfig::Diagnostic| d.message().starts_with("local files failed");
    assert!(notes.iter().any(said), "{notes:?}");
}

/// An app that prefers the daemon opens on files when no daemon answers.
#[test]
fn a_daemon_preference_falls_back_to_files_when_none_answers() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, _) = source(tmp.path(), &prefers_daemon());
    let first = open(&source, NS).unwrap();
    assert!(!first.from_daemon(), "no bus, so files at once");
    drop(first);

    // A bus, but no daemon owns the name.
    reach(&source);
    let ticket = source.request_namespace(NS);
    let call = source.take_calls(Bus::Session).remove(0);
    let unknown = "org.freedesktop.DBus.Error.ServiceUnknown";
    source.deliver(Bus::Session, failed(call.id, unknown));
    let settings = opened(&source, ticket).unwrap();
    assert!(!settings.from_daemon());
    let notes = settings.diagnostics();
    let said = |d: &mxconfig::Diagnostic| d.message().starts_with("mxconfigd failed");
    assert!(notes.iter().any(said), "{notes:?}");
}

/// A refusal is an answer, so the other backend is not tried.
#[test]
fn a_refusal_does_not_fall_back() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, _) = source(tmp.path(), &prefers_daemon());
    reach(&source);
    let ticket = source.request_namespace(NS);
    let call = source.take_calls(Bus::Session).remove(0);
    source.deliver(Bus::Session, failed(call.id, ACCESS_DENIED));
    let refused = opened(&source, ticket).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Denied, "{refused}");
}

/// When both backends fail, the error says why for each.
#[test]
fn when_both_backends_fail_the_error_names_both() {
    let tmp = tempfile::tempdir().unwrap();
    let source = Source::with_roots(Roots::under(tmp.path()));
    let error = open(&source, NS).unwrap_err();
    let text = error.to_string();
    let both = text.contains("no schema installed") && text.contains("cannot reach mxconfigd");
    assert!(both, "{text}");
}

/// An app that wants only the daemon never reads its files itself.
#[test]
fn a_daemon_only_namespace_is_not_read_from_files() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, _) = source(tmp.path(), &remote(SCHEMA));
    let refused = open(&source, NS).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Unreachable(Bus::Session));
}

/// The demo namespace at v1, before `colors.radius` was renamed.
const V1: &str = r#"
namespace = "org.mechanix.demo"
version = 1

[groups.colors]
label = "Colors"

[keys.colors.radius]
type = "integer"
default = 8
label = "Radius"
description = "d"
"#;

/// An app source never follows schema changes. Only a serving one does.
#[test]
fn an_app_source_keeps_the_schema_it_opened_with() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, roots) = source(tmp.path(), V1);
    let settings = open(&source, NS).unwrap();
    let file = common::install(&roots, NS, SCHEMA);
    source.touched(&file);
    source.rescan();
    assert!(!detached(&source.outcomes(), NS));
    assert_eq!(settings.describe().version(), 1);
}

/// `WITH_ASSET` at version 2, as an update would install it.
fn with_asset_v2() -> String {
    WITH_ASSET.replace("version = 1", "version = 2")
}

/// A typed client that the daemon answers with another schema version doesn't open.
#[test]
fn a_typed_client_refuses_another_version_at_open() {
    let tmp = tempfile::tempdir().unwrap();
    let source = Source::with_roots(Roots::under(tmp.path()));
    reach(&source);
    let ticket = source.request::<Remote>();
    let id = source.take_calls(Bus::Session)[0].id;
    let reply = subscribed(&with_asset_v2(), 0, &one("level", Value::Integer(4)), &[]);
    source.deliver(Bus::Session, answer(id, reply));
    let refused = opened(&source, ticket).err().expect("refused");
    assert_eq!(refused.kind(), ErrorKind::VersionConflict, "{refused}");
    assert_eq!(refused.other_version(), Some(2), "the daemon serves v2");

    // The daemon's own refusal reads the same way, without the version.
    let ticket = source.request::<Remote>();
    let id = source.take_calls(Bus::Session)[0].id;
    source.deliver(Bus::Session, failed(id, VERSION_CONFLICT));
    let refused = opened(&source, ticket).err().expect("refused");
    assert_eq!(refused.kind(), ErrorKind::VersionConflict, "{refused}");
    assert_eq!(refused.other_version(), None);
}

/// A restart onto another version detaches the handle.
#[test]
fn a_restart_onto_another_version_detaches_the_handle() {
    let tmp = tempfile::tempdir().unwrap();
    let source = Source::with_roots(Roots::under(tmp.path()));
    reach(&source);
    let ticket = source.request::<Remote>();
    let id = source.take_calls(Bus::Session)[0].id;
    let held = one("level", Value::Integer(4));
    source.deliver(
        Bus::Session,
        answer(id, subscribed(WITH_ASSET, 1, &held, &[])),
    );
    let demo = opened(&source, ticket).unwrap();

    // A restarted daemon serves v2, which the client wasn't built for.
    source.deliver(Bus::Session, owner(""));
    source.deliver(Bus::Session, owner(":1.2"));
    let call = source.take_calls(Bus::Session).remove(0);
    assert_eq!(schema(&call), REMOTE, "it says which version it holds");
    let moved = one("level", Value::Integer(9));
    source.deliver(
        Bus::Session,
        answer(call.id, subscribed(&with_asset_v2(), 0, &moved, &[])),
    );
    let outcomes = source.outcomes();
    let detached =
        |o: &Outcome| matches!(o, Outcome::Detached { namespace, .. } if namespace == NS);
    assert!(outcomes.iter().any(detached), "{outcomes:?}");
    assert!(
        !outcomes.iter().any(|o| matches!(o, Outcome::Changed(_))),
        "{outcomes:?}"
    );

    // It keeps the values it had, never defaults.
    assert_eq!(demo.0.get("level"), Some(Value::Integer(4)));
    let said = |d: &mxconfig::Diagnostic| d.message().contains("detached");
    assert!(demo.0.diagnostics().iter().any(said));
    let refused = demo.0.write(&[Edit::set("level", 5)]).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Detached, "{refused}");
    assert!(
        source.take_calls(Bus::Session).is_empty(),
        "nothing is sent"
    );

    // Later signals from the new daemon are ignored.
    let event = changed(NS, 1, &moved);
    let Event::Signal { args, .. } = event else {
        unreachable!()
    };
    source.deliver(Bus::Session, from(":1.2", INTERFACE, CHANGED, args));
    assert_eq!(demo.0.get("level"), Some(Value::Integer(4)));
}

/// A resubscribe the daemon refuses for another version detaches the handle too.
#[test]
fn a_refused_resubscribe_detaches_the_handle() {
    let tmp = tempfile::tempdir().unwrap();
    let source = Source::with_roots(Roots::under(tmp.path()));
    reach(&source);
    let ticket = source.request::<Remote>();
    let id = source.take_calls(Bus::Session)[0].id;
    source.deliver(
        Bus::Session,
        answer(id, subscribed(WITH_ASSET, 1, &BTreeMap::new(), &[])),
    );
    let demo = opened(&source, ticket).unwrap();
    source.deliver(Bus::Session, owner(""));
    source.deliver(Bus::Session, owner(":1.2"));
    let id = source.take_calls(Bus::Session)[0].id;
    source.deliver(Bus::Session, failed(id, VERSION_CONFLICT));
    let detached = |o: &Outcome| matches!(o, Outcome::Detached { .. });
    assert!(source.outcomes().iter().any(detached));
    assert_eq!(
        demo.0.write(&[Edit::set("level", 5)]).unwrap_err().kind(),
        ErrorKind::Detached
    );
}

/// A client without a schema of its own reads with the one the daemon serves.
#[test]
fn a_client_without_a_schema_takes_the_served_one() {
    let tmp = tempfile::tempdir().unwrap();
    let source = Source::with_roots(Roots::under(tmp.path()));
    reach(&source);
    let ticket = source.request_namespace(NS);
    let id = source.take_calls(Bus::Session)[0].id;
    let newer = SCHEMA.replace("version = 2", "version = 3");
    source.deliver(
        Bus::Session,
        answer(id, subscribed(&newer, 0, &BTreeMap::new(), &[])),
    );
    let outcomes = source.outcomes();
    assert!(!detached(&outcomes, NS));
    let settings = outcomes
        .iter()
        .find_map(|o| ticket.opened(o))
        .unwrap()
        .unwrap();
    assert_eq!(settings.describe().version(), 3);
}

/// The error the daemon sends when another schema version holds the namespace.
const VERSION_CONFLICT: &str = "org.mechanix.MxConfig.Error.VersionConflict";

/// A client reading only names another version left alone opens through the daemon, but only reads.
#[test]
fn a_client_at_another_version_reads_untouched_names_only() {
    let tmp = tempfile::tempdir().unwrap();
    let source = Source::with_roots(Roots::under(tmp.path()));
    let verbose =
        "[keys.verbose]\ntype = \"bool\"\ndefault = false\nlabel = \"V\"\ndescription = \"d\"\n";
    source.embed(
        NS,
        &remote(&format!("namespace = \"{NS}\"\nversion = 1\n{verbose}")),
    );
    reach(&source);
    let ticket = source.request_namespace(NS);
    let id = source.take_calls(Bus::Session)[0].id;
    let reply = subscribed(SCHEMA, 1, &one("verbose", Value::Boolean(true)), &[]);
    source.deliver(Bus::Session, answer(id, reply));
    let settings = opened(&source, ticket).unwrap();
    assert_eq!(settings.other_version(), Some(2));
    assert_eq!(settings.get("verbose"), Some(Value::Boolean(true)));
    let refused = settings.write(&[Edit::set("verbose", false)]).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::VersionConflict, "{refused}");
    assert!(
        source.take_calls(Bus::Session).is_empty(),
        "nothing is sent"
    );
}
