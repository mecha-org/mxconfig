//! Shared fixtures for the integration tests.

#![allow(dead_code, reason = "each test binary uses a different part")]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use mxconfig::host::Roots;
use mxconfig::host::protocol::{BUS_NAME, CHANGED, DBUS, INTERFACE, NAME_OWNER_CHANGED};
use mxconfig::runtime::wire::{Arg, Call};
use mxconfig::runtime::{Drive, Event};
use mxconfig::{Bus, Change, Edit, Error, FromSettings, Outcome, Schema, Settings, Source};
use mxconfig::{Ticket, Value};

/// The demo namespace at v2.
pub const SCHEMA: &str = r##"
namespace = "org.mechanix.demo"
version = 2

[groups.colors]
label = "Colors"
order = 10

[keys.colors.accent]
type = "string"
default = "#7aa2f7"
label = "Accent"
description = "The accent color."

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
description = "Theme mode."

[keys.verbose]
type = "bool"
default = false
label = "Verbose"
description = "Verbose logging."

[keys.nickname]
type = "string"
label = "Nickname"
description = "Display name; unset until the user picks one."

[[migration]]
from = 1
to = 2
ops = [
  { op = "rename", from = "colors.radius", to = "colors.corner_radius" },
]
"##;

pub fn schema() -> Schema {
    Schema::parse(SCHEMA).expect("v2 schema parses")
}

/// A source whose only layer is the user directory `dir`.
pub fn in_dir(dir: impl Into<PathBuf>) -> Source {
    Source::with_roots(Roots {
        user_dir: Some(dir.into()),
        ..Roots::default()
    })
}

/// Open `schema` over `dir` alone, on a source of its own.
pub fn load_in(dir: impl Into<PathBuf>, schema: Schema) -> Result<Settings, Error> {
    in_dir(dir).open_schema(schema)
}

/// The result `ticket` waits for. Drains `source`'s outcomes.
pub fn opened<T: FromSettings>(source: &Source, ticket: Ticket<T>) -> Result<T, Error> {
    source
        .outcomes()
        .iter()
        .find_map(|o| ticket.opened(o))
        .expect("an Opened outcome")
}

/// Open `namespace` on `source`. A file open answers at once.
pub fn open(source: &Source, namespace: &str) -> Result<Settings, Error> {
    opened(source, source.request_namespace(namespace))
}

/// Write `text` to `path`, making its parent directories.
pub fn put(path: &Path, text: impl AsRef<[u8]>) -> PathBuf {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
    path.to_path_buf()
}

/// Install `text` as `namespace`'s schema in `roots`.
pub fn install(roots: &Roots, namespace: &str, text: &str) -> PathBuf {
    put(
        &roots.schema_dirs[0].join(format!("{namespace}.toml")),
        text,
    )
}

/// A schema for `namespace` at `version`, then `body`.
pub fn at(namespace: &str, version: u32, body: &str) -> String {
    format!("namespace = \"{namespace}\"\nversion = {version}\n{body}")
}

/// A key declaration for `path` of type `ty`.
pub fn key(path: &str, ty: &str) -> String {
    format!("[keys.{path}]\ntype = \"{ty}\"\nlabel = \"L\"\ndescription = \"d\"\n")
}

/// A migration edge with `ops`, one inline table each.
pub fn edge(from: u32, to: u32, ops: &[&str]) -> String {
    let ops = ops.join(",\n  ");
    format!("\n[[migration]]\nfrom = {from}\nto = {to}\nops = [\n  {ops},\n]\n")
}

/// Whether `outcomes` say `namespace`'s handle detached.
pub fn detached(outcomes: &[Outcome], namespace: &str) -> bool {
    let gone = |o: &Outcome| matches!(o, Outcome::Detached { namespace: n, .. } if n == namespace);
    outcomes.iter().any(gone)
}

/// The version and edges recorded beside `file`, if it has a state file.
pub fn recorded(file: &Path) -> Option<(i64, Vec<(i64, i64)>)> {
    let mut state = file.as_os_str().to_owned();
    state.push(".state");
    let state: toml::Table = fs::read_to_string(state).ok()?.parse().unwrap();
    let pair = |m: &toml::Value| {
        let end = |name: &str| m[name].as_integer().unwrap();
        (end("from"), end("to"))
    };
    let applied = state.get("applied").and_then(toml::Value::as_array);
    let edges = applied.map(|a| a.iter().map(pair).collect());
    Some((state["version"].as_integer()?, edges.unwrap_or_default()))
}

pub fn int(n: i64) -> Value {
    Value::Integer(n)
}

pub fn text(s: &str) -> Value {
    Value::String(s.into())
}

/// Write helpers that go through a handle's [`Source`].
pub trait Through {
    fn apply(&self, edits: &[Edit]) -> Result<Vec<Change>, Error>;
    fn set_asset(&self, id: &str, bytes: &[u8]) -> Result<Vec<Change>, Error>;
    fn remove_asset(&self, id: &str) -> Result<Vec<Change>, Error>;
    /// Rescan after files were changed outside.
    fn reload(&self) -> Vec<Change>;

    fn set(&self, key: &str, value: Value) -> Result<Vec<Change>, Error> {
        self.apply(&[Edit::set(key, value)])
    }

    fn remove(&self, key: &str) -> Result<Vec<Change>, Error> {
        self.apply(&[Edit::remove(key)])
    }
}

impl Through for Settings {
    fn apply(&self, edits: &[Edit]) -> Result<Vec<Change>, Error> {
        let ticket = self.write(edits)?;
        landed(self.source(), |o| ticket.written(o))
    }

    fn set_asset(&self, id: &str, bytes: &[u8]) -> Result<Vec<Change>, Error> {
        let ticket = self.write_asset(id, bytes)?;
        landed(self.source(), |o| ticket.written(o))
    }

    fn remove_asset(&self, id: &str) -> Result<Vec<Change>, Error> {
        let ticket = self.clear_asset(id)?;
        landed(self.source(), |o| ticket.written(o))
    }

    fn reload(&self) -> Vec<Change> {
        self.source().rescan();
        changes(self.source().outcomes())
    }
}

/// The write's result and the changes it made.
fn landed(
    source: &Source,
    written: impl Fn(&Outcome) -> Option<Result<(), Error>>,
) -> Result<Vec<Change>, Error> {
    let outcomes = source.outcomes();
    outcomes
        .iter()
        .find_map(written)
        .expect("a local write lands at once")?;
    Ok(changes(outcomes))
}

fn changes(outcomes: Vec<Outcome>) -> Vec<Change> {
    outcomes
        .into_iter()
        .filter_map(|o| match o {
            Outcome::Changed(change) => Some(change),
            _ => None,
        })
        .collect()
}

/// The fake daemon's unique name.
pub const DAEMON: &str = ":1.1";

/// Reach the session bus, where `DAEMON` owns mxconfigd's name.
pub fn reach(source: &Source) {
    assert!(reach_bus(source, Bus::Session).is_empty());
}

/// Reach `bus`, and return the calls sent besides the owner query.
pub fn reach_bus(source: &Source, bus: Bus) -> Vec<Call> {
    source.reach(bus);
    let mut calls = source.take_calls(bus);
    assert_eq!(calls.first().map(|call| call.member), Some("GetNameOwner"));
    let id = calls.remove(0).id;
    let args = vec![Arg::Str(DAEMON.into())];
    source.deliver(bus, Event::Reply { id, args });
    calls
}

/// `schema`, asking for the daemon only.
pub fn remote(schema: &str) -> String {
    format!("backend = \"daemon-only\"\n{schema}")
}

/// The members of `calls`, in order.
pub fn members(calls: &[Call]) -> Vec<&'static str> {
    calls.iter().map(|call| call.member).collect()
}

/// The daemon's error reply `name` to call `id`.
pub fn failed(id: u64, name: &str) -> Event {
    let (name, message) = (name.into(), "no".into());
    Event::Failed { id, name, message }
}

/// The bus saying `owner` now owns the daemon's name.
pub fn owner(owner: &str) -> Event {
    let args = [BUS_NAME, "", owner].map(|text| Arg::Str(text.into()));
    Event::Signal {
        sender: DBUS.into(),
        interface: DBUS.into(),
        member: NAME_OWNER_CHANGED.into(),
        args: args.to_vec(),
    }
}

/// The error polkit's refusal comes back as.
pub const ACCESS_DENIED: &str = "org.freedesktop.DBus.Error.AccessDenied";

/// The daemon's reply to call `id`.
pub fn answer(id: u64, args: Vec<Arg>) -> Event {
    Event::Reply { id, args }
}

/// A signal from `sender`.
pub fn from(sender: &str, interface: &str, member: &str, args: Vec<Arg>) -> Event {
    Event::Signal {
        sender: sender.into(),
        interface: interface.into(),
        member: member.into(),
        args,
    }
}

/// A revision and values as the daemon sends them: `t` then `a{sv}` of TOML text.
pub fn state(revision: u64, values: &BTreeMap<String, Value>) -> Vec<Arg> {
    let texts = values.iter().map(|(k, v)| (k.clone(), v.to_string()));
    raw_state(revision, texts.collect())
}

/// A revision and values given as raw TOML text, which may not parse.
pub fn raw_state(revision: u64, texts: Vec<(String, String)>) -> Vec<Arg> {
    let text = |text: String| Arg::Variant(Box::new(Arg::Str(text)));
    let values = texts.into_iter().map(|(k, t)| (Arg::Str(k), text(t)));
    vec![Arg::U64(revision), Arg::Dict(values.collect())]
}

/// The daemon's `Changed` with the whole state at `revision`.
pub fn changed(namespace: &str, revision: u64, values: &BTreeMap<String, Value>) -> Event {
    let args = [vec![Arg::Str(namespace.into())], state(revision, values)].concat();
    from(DAEMON, INTERFACE, CHANGED, args)
}

/// A read-only fd on `path`, as the daemon passes one.
pub fn fd(path: &Path) -> Arg {
    let file = fs::File::open(path).unwrap();
    Arg::Fd(std::sync::Arc::new(file.into()))
}

/// A `Subscribe` reply: schema, revision, values and `(id, path)` assets.
pub fn subscribed(
    schema: &str,
    revision: u64,
    values: &BTreeMap<String, Value>,
    assets: &[(&str, &Path)],
) -> Vec<Arg> {
    let text = |value: &Value| Arg::Variant(Box::new(Arg::Str(value.to_string())));
    let values = values.iter().map(|(k, v)| (Arg::Str(k.clone()), text(v)));
    let asset = |(id, path): &(&str, &Path)| {
        let path_text = Arg::Str(path.display().to_string());
        Arg::Struct(vec![Arg::Str(id.to_string()), path_text, fd(path)])
    };
    // Any open file stands in for the daemon's subscription pipe.
    let subscription = Arg::file(b"").unwrap();
    vec![
        subscription,
        Arg::Str(schema.into()),
        Arg::U64(revision),
        Arg::Dict(values.collect()),
        Arg::Array("(ssh)".into(), assets.iter().map(asset).collect()),
    ]
}
