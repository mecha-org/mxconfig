//! The D-Bus protocol mxconfig and mxconfigd share.

use mxconfig_schema::value::{self, Value};

/// mxconfigd's bus name, on the session and system bus.
pub const BUS_NAME: &str = "org.mechanix.MxConfig";

/// The object path of the interface.
pub const OBJECT_PATH: &str = "/org/mechanix/MxConfig";

/// The interface name, the same as the bus name.
pub const INTERFACE: &str = BUS_NAME;

/// The polkit action for writing a privileged namespace.
pub const WRITE_ACTION: &str = "org.mechanix.mxconfig.write";

/// The message bus's own name, which reports name owner changes.
pub const DBUS: &str = "org.freedesktop.DBus";

/// The message bus's object path, where `AddMatch` is called.
pub const DBUS_PATH: &str = "/org/freedesktop/DBus";

/// `Changed(s namespace, t revision, a{sv} values)`.
pub const CHANGED: &str = "Changed";

/// `AssetChanged(s namespace, s id, a(sh) file)`.
pub const ASSET_CHANGED: &str = "AssetChanged";

/// `NameOwnerChanged(s name, s old, s new)`, from the message bus.
pub const NAME_OWNER_CHANGED: &str = "NameOwnerChanged";

/// `GetNameOwner(s name) → s`, on the message bus.
pub const GET_NAME_OWNER: &str = "GetNameOwner";

/// `AddMatch(s rule)`, on the message bus.
pub const ADD_MATCH: &str = "AddMatch";

/// `Subscribe(s namespace, s schema, s app) → (h, s, t, a{sv}, a(ssh))`.
pub const SUBSCRIBE: &str = "Subscribe";

/// `Apply(s namespace, a(sbs) edits, s app) → (t, a{sv})`.
pub const APPLY: &str = "Apply";

/// `WriteAsset(h file, s namespace, s id, s app)`.
pub const WRITE_ASSET: &str = "WriteAsset";

/// `ClearAsset(s namespace, s id, s app)`.
pub const CLEAR_ASSET: &str = "ClearAsset";

/// `ListNamespaces(s app) → as`.
pub const LIST_NAMESPACES: &str = "ListNamespaces";

/// The signature of one [`Entry`].
pub const ENTRY_SIGNATURE: &str = "(sbs)";

/// The largest asset file a client may send, in bytes.
pub const MAX_ASSET: u64 = 64 << 20;

/// An edit on the wire: key, whether it is set, and TOML text.
pub type Entry = (String, bool, String);

/// The entry for `key`, set to `value` or unset.
pub fn entry(key: &str, value: Option<&Value>) -> Entry {
    let text = value.map(ToString::to_string).unwrap_or_default();
    (key.to_string(), value.is_some(), text)
}

/// An entry's key and value, `None` when unset.
pub fn decode((key, set, text): Entry) -> Result<(String, Option<Value>), mxconfig_schema::Error> {
    let value = set.then(|| value::from_toml_str(&text)).transpose()?;
    Ok((key, value))
}

/// What a D-Bus error reply from mxconfigd means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// Policy, polkit or a managed key refused it.
    Denied,
    /// A key, value or schema was rejected.
    Invalid,
    /// The namespace is not served.
    Unknown,
    /// Another schema version holds the namespace.
    Version,
    /// No daemon answered, or the call was never sent.
    Unreachable,
    /// Anything else.
    Other,
}

const ACCESS_DENIED: &str = "org.freedesktop.DBus.Error.AccessDenied";
const INVALID_ARGS: &str = "org.freedesktop.DBus.Error.InvalidArgs";
const UNKNOWN_OBJECT: &str = "org.freedesktop.DBus.Error.UnknownObject";
const SERVICE_UNKNOWN: &str = "org.freedesktop.DBus.Error.ServiceUnknown";
const FAILED: &str = "org.freedesktop.DBus.Error.Failed";
const NAME_HAS_NO_OWNER: &str = "org.freedesktop.DBus.Error.NameHasNoOwner";
/// The error name for a call that got no reply.
pub const NO_REPLY: &str = "org.freedesktop.DBus.Error.NoReply";
const DISCONNECTED: &str = "org.freedesktop.DBus.Error.Disconnected";
const VERSION_CONFLICT: &str = "org.mechanix.MxConfig.Error.VersionConflict";

impl Failure {
    /// The D-Bus error name for this failure.
    pub fn name(self) -> &'static str {
        match self {
            Failure::Denied => ACCESS_DENIED,
            Failure::Invalid => INVALID_ARGS,
            Failure::Unknown => UNKNOWN_OBJECT,
            Failure::Version => VERSION_CONFLICT,
            Failure::Unreachable => SERVICE_UNKNOWN,
            Failure::Other => FAILED,
        }
    }

    /// The failure an error name means. An empty name means never sent.
    pub fn of(name: &str) -> Failure {
        match name {
            ACCESS_DENIED => Failure::Denied,
            INVALID_ARGS => Failure::Invalid,
            UNKNOWN_OBJECT => Failure::Unknown,
            VERSION_CONFLICT => Failure::Version,
            "" | SERVICE_UNKNOWN | NAME_HAS_NO_OWNER | NO_REPLY | DISCONNECTED => {
                Failure::Unreachable
            }
            _ => Failure::Other,
        }
    }

    /// Whether `name` means a sent call got no answer, so it may have landed.
    pub fn unanswered(name: &str) -> bool {
        matches!(name, NO_REPLY | DISCONNECTED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_failure_round_trips_through_its_name() {
        use Failure::{Denied, Invalid, Other, Unknown, Unreachable, Version};
        for failure in [Denied, Invalid, Unknown, Version, Unreachable, Other] {
            assert_eq!(Failure::of(failure.name()), failure);
        }
    }

    #[test]
    fn an_entry_round_trips_set_and_unset() {
        let set = entry("a.b", Some(&Value::Integer(3)));
        assert_eq!(set, ("a.b".into(), true, "3".into()));
        assert_eq!(
            decode(set).unwrap(),
            ("a.b".into(), Some(Value::Integer(3)))
        );
        assert_eq!(decode(entry("a.b", None)).unwrap(), ("a.b".into(), None));
        assert!(decode(("a.b".into(), true, "3 =".into())).is_err());
    }
}
