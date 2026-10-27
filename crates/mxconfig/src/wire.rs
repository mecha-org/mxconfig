//! The D-Bus protocol: calls, replies and signals as plain values.

use std::collections::BTreeMap;
use std::os::fd::OwnedFd;
use std::sync::Arc;

use crate::error::{Error, Kind};
use crate::host::protocol::{self, Failure};
use crate::settings::Edit;
use crate::transport::{Bus, Kept, Served};
use mxconfig_schema::value::{self, Value};

use protocol::{APPLY, ASSET_CHANGED, BUS_NAME, CHANGED, CLEAR_ASSET, DBUS, DBUS_PATH};
use protocol::{ENTRY_SIGNATURE, GET_NAME_OWNER, INTERFACE, NAME_OWNER_CHANGED, OBJECT_PATH};
use protocol::{SUBSCRIBE, WRITE_ASSET};

/// A D-Bus value. Transports convert it to and from their own types.
#[derive(Debug, Clone)]
pub enum Arg {
    Str(String),
    U64(u64),
    Bool(bool),
    Fd(Arc<OwnedFd>),
    /// A descriptor the transport keeps open itself, as it can't hand it over.
    Kept(Kept),
    /// An array, with its element signature for encoding an empty one.
    Array(String, Vec<Arg>),
    Struct(Vec<Arg>),
    /// A dictionary, as key and value pairs.
    Dict(Vec<(Arg, Arg)>),
    Variant(Box<Arg>),
    /// A value the transport could not convert, such as an fd it could not duplicate.
    Unreadable,
}

impl Arg {
    /// An fd on an anonymous file holding `bytes`.
    #[doc(hidden)]
    pub fn file(bytes: &[u8]) -> std::io::Result<Arg> {
        Ok(Arg::Fd(Served::from_bytes(bytes)?.shared()))
    }

    fn into_str(self) -> Option<String> {
        match self {
            Arg::Str(text) => Some(text),
            _ => None,
        }
    }

    fn into_u64(self) -> Option<u64> {
        match self {
            Arg::U64(n) => Some(n),
            _ => None,
        }
    }

    /// The items of an array or the fields of a struct.
    fn into_items(self) -> Option<Vec<Arg>> {
        match self {
            Arg::Array(_, items) | Arg::Struct(items) => Some(items),
            _ => None,
        }
    }

    /// The value inside a variant, or the arg itself.
    fn into_inner(self) -> Arg {
        match self {
            Arg::Variant(inner) => *inner,
            other => other,
        }
    }
}

/// A method call for a transport to send.
#[derive(Debug, Clone)]
pub struct Call {
    /// The request id to deliver the reply under.
    pub id: u64,
    pub destination: &'static str,
    pub path: &'static str,
    pub interface: &'static str,
    pub member: &'static str,
    pub args: Vec<Arg>,
    /// Whether polkit may prompt the user.
    pub interactive: bool,
    /// Whether the reply's first argument is a descriptor the source keeps open.
    pub keep: bool,
}

impl Call {
    /// A call to mxconfigd.
    fn daemon(id: u64, member: &'static str, args: Vec<Arg>) -> Call {
        Call {
            id,
            destination: BUS_NAME,
            path: OBJECT_PATH,
            interface: INTERFACE,
            member,
            args,
            interactive: false,
            keep: false,
        }
    }

    /// Ask the message bus who owns mxconfigd's name.
    pub(crate) fn owner(id: u64) -> Call {
        Call {
            id,
            destination: DBUS,
            path: DBUS_PATH,
            interface: DBUS,
            member: GET_NAME_OWNER,
            args: vec![Arg::Str(BUS_NAME.into())],
            interactive: false,
            keep: false,
        }
    }

    /// Open or resubscribe `namespace` as `app`, registering the app's own `schema` if any.
    pub(crate) fn subscribe(id: u64, app: &str, namespace: &str, schema: Option<&str>) -> Call {
        let args = vec![
            Arg::Str(namespace.into()),
            Arg::Str(schema.unwrap_or_default().into()),
            Arg::Str(app.into()),
        ];
        // The reply's descriptor is the subscription. Closing it ends the subscription.
        Call {
            keep: true,
            ..Call::daemon(id, SUBSCRIBE, args)
        }
    }

    /// Apply `edits` atomically as `app`. Polkit may prompt for a privileged namespace.
    pub(crate) fn apply(id: u64, app: &str, namespace: &str, edits: &[Edit]) -> Call {
        let edits = edits.iter().map(edit_arg).collect();
        let args = vec![
            Arg::Str(namespace.into()),
            Arg::Array(ENTRY_SIGNATURE.into(), edits),
            Arg::Str(app.into()),
        ];
        Call {
            interactive: true,
            ..Call::daemon(id, APPLY, args)
        }
    }

    /// Set the user's copy of asset `asset` to `file` as `app`, or clear it if `None`.
    pub(crate) fn write_asset(
        id: u64,
        app: &str,
        namespace: &str,
        asset: &str,
        file: Option<&Served>,
    ) -> Call {
        let (namespace, asset) = (Arg::Str(namespace.into()), Arg::Str(asset.into()));
        let app = Arg::Str(app.into());
        match file {
            // The fd is first: `package:dbus` misaligns one after other args.
            Some(file) => {
                let args = vec![Arg::Fd(file.shared()), namespace, asset, app];
                Call::daemon(id, WRITE_ASSET, args)
            }
            None => Call::daemon(id, CLEAR_ASSET, vec![namespace, asset, app]),
        }
    }
}

/// A signal match rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rule {
    pub sender: &'static str,
    pub interface: &'static str,
    pub member: &'static str,
    /// The required first argument, if any.
    pub arg0: Option<&'static str>,
}

impl Rule {
    /// The rule as an `AddMatch` string.
    pub fn text(&self) -> String {
        let (sender, interface, member) = (self.sender, self.interface, self.member);
        let rule =
            format!("type='signal',sender='{sender}',interface='{interface}',member='{member}'");
        match self.arg0 {
            Some(arg0) => format!("{rule},arg0='{arg0}'"),
            None => rule,
        }
    }
}

/// The rules a transport adds to receive the daemon's signals.
pub const RULES: [Rule; 3] = [
    Rule {
        sender: BUS_NAME,
        interface: INTERFACE,
        member: CHANGED,
        arg0: None,
    },
    Rule {
        sender: BUS_NAME,
        interface: INTERFACE,
        member: ASSET_CHANGED,
        arg0: None,
    },
    Rule {
        sender: DBUS,
        interface: DBUS,
        member: NAME_OWNER_CHANGED,
        arg0: Some(BUS_NAME),
    },
];

/// A subscription: the descriptor that keeps it alive while open.
#[derive(Debug)]
pub(crate) struct Subscription(#[expect(dead_code, reason = "held for its drop")] Arg);

/// A namespace's state as a `Subscribe` reply holds it.
pub(crate) struct Snapshot {
    pub(crate) subscription: Subscription,
    /// The schema the daemon serves the namespace with, as TOML text.
    pub(crate) schema: String,
    pub(crate) state: State,
    /// The files the daemon opened for each set asset.
    pub(crate) assets: BTreeMap<String, Served>,
}

/// A revision and every value set at it.
#[derive(Debug)]
pub(crate) struct State {
    pub(crate) revision: u64,
    pub(crate) values: BTreeMap<String, Value>,
}

/// The owner's unique name from a `GetNameOwner` reply.
pub(crate) fn owner(args: Vec<Arg>) -> Option<String> {
    let [owner] = <[Arg; 1]>::try_from(args).ok()?;
    owner.into_str()
}

/// Decode a `Subscribe` reply.
pub(crate) fn snapshot(args: Vec<Arg>) -> Result<Snapshot, Error> {
    let bad = || Error::from(Kind::Daemon(Failure::Other, "a bad Subscribe reply".into()));
    let Ok([subscription, schema, revision, values, assets]) = <[Arg; 5]>::try_from(args) else {
        return Err(bad());
    };
    if !matches!(subscription, Arg::Fd(_) | Arg::Kept(_)) {
        return Err(bad());
    }
    let mut snapshot = Snapshot {
        subscription: Subscription(subscription),
        schema: schema.into_str().ok_or_else(bad)?,
        state: state(revision, values).ok_or_else(bad)??,
        assets: BTreeMap::new(),
    };
    for asset in assets.into_items().ok_or_else(bad)? {
        let [id, path, fd] = fields(asset).ok_or_else(bad)?;
        let id = id.into_str().ok_or_else(bad)?;
        snapshot
            .assets
            .insert(id, served(path, fd).ok_or_else(bad)?);
    }
    Ok(snapshot)
}

/// Decode an `Apply` reply: the revision the write produced, and the values.
pub(crate) fn applied(args: Vec<Arg>) -> Result<State, Error> {
    let bad = || Error::from(Kind::Daemon(Failure::Other, "a bad Apply reply".into()));
    let Ok([revision, values]) = <[Arg; 2]>::try_from(args) else {
        return Err(bad());
    };
    state(revision, values).ok_or_else(bad)?
}

/// Decode a revision and an `a{sv}` of TOML text. `None` if malformed.
fn state(revision: Arg, values: Arg) -> Option<Result<State, Error>> {
    let revision = revision.into_u64()?;
    let Arg::Dict(pairs) = values else {
        return None;
    };
    let mut values = BTreeMap::new();
    for (key, text) in pairs {
        let text = text.into_inner().into_str()?;
        match value::from_toml_str(&text) {
            Ok(value) => values.insert(key.into_str()?, value),
            Err(e) => return Some(Err(e.into())),
        };
    }
    Some(Ok(State { revision, values }))
}

/// A signal from the bus, decoded.
#[derive(Debug)]
pub(crate) enum Heard {
    Changed {
        namespace: String,
        state: State,
    },
    AssetChanged {
        namespace: String,
        id: String,
        file: Option<Served>,
    },
    /// An `AssetChanged` that could not be decoded, with its namespace if known.
    Unreadable {
        namespace: Option<String>,
    },
    /// The daemon's bus name has a new owner. Empty when it has none.
    Owner(String),
}

/// Decode a signal. `None` if it is not one mxconfig listens to.
pub(crate) fn heard(interface: &str, member: &str, args: Vec<Arg>) -> Option<Heard> {
    match (interface, member) {
        (DBUS, NAME_OWNER_CHANGED) => {
            let [name, _, owner] = <[Arg; 3]>::try_from(args).ok()?;
            (name.into_str()? == BUS_NAME).then_some(Heard::Owner(owner.into_str()?))
        }
        // Skip an unreadable state, since the next one is whole.
        (INTERFACE, CHANGED) => {
            let [namespace, revision, values] = <[Arg; 3]>::try_from(args).ok()?;
            Some(Heard::Changed {
                namespace: namespace.into_str()?,
                state: state(revision, values)?.ok()?,
            })
        }
        (INTERFACE, ASSET_CHANGED) => Some(asset_changed(args)),
        _ => None,
    }
}

/// Decode an `AssetChanged`. An unreadable one names its namespace if it can.
fn asset_changed(args: Vec<Arg>) -> Heard {
    let mut args = args.into_iter();
    let namespace = args.next().and_then(Arg::into_str);
    let decode = |namespace: &str| {
        let [id, file] = <[Arg; 2]>::try_from(args.collect::<Vec<_>>()).ok()?;
        let file = match file.into_items()?.into_iter().next() {
            Some(file) => Some(fields(file).and_then(|[path, fd]| served(path, fd))?),
            None => None,
        };
        let (namespace, id) = (namespace.into(), id.into_str()?);
        Some(Heard::AssetChanged {
            namespace,
            id,
            file,
        })
    };
    let decoded = namespace.as_deref().and_then(decode);
    decoded.unwrap_or(Heard::Unreadable { namespace })
}

/// The `N` fields of a struct.
fn fields<const N: usize>(arg: Arg) -> Option<[Arg; N]> {
    <[Arg; N]>::try_from(arg.into_items()?).ok()
}

/// A file the daemon passed: its path and fd.
fn served(path: Arg, fd: Arg) -> Option<Served> {
    let Arg::Fd(fd) = fd else { return None };
    Some(Served::from_shared(path.into_str()?, fd))
}

/// Convert a D-Bus error reply from `bus` to an [`Error`], for a `write` or not.
pub(crate) fn failure(bus: Bus, error_name: &str, text: String, write: bool) -> Error {
    let kind = match Failure::of(error_name) {
        // A write that got no answer may have landed.
        _ if write && Failure::unanswered(error_name) => Kind::Unconfirmed(bus),
        Failure::Denied => Kind::Access(text),
        Failure::Version => Kind::VersionConflict {
            why: text,
            other: None,
        },
        Failure::Unreachable => Kind::Unreachable(bus),
        failure => Kind::Daemon(failure, text),
    };
    kind.into()
}

/// `edit` as a wire entry, the struct `(sbs)`.
fn edit_arg(edit: &Edit) -> Arg {
    let (key, set, text) = protocol::entry(edit.key(), edit.value.as_ref());
    Arg::Struct(vec![Arg::Str(key), Arg::Bool(set), Arg::Str(text)])
}
