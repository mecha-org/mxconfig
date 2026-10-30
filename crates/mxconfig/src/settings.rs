//! The settings handle for one open namespace.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use crate::error::{Diagnostic, Error};
use crate::manifest::{Access, Manifest};
use crate::paths;
use crate::resolve::{Origin, Resolved};
use crate::source::{Detach, Source};
use crate::space::{Key, Space};
use crate::transport::Served;
use mxconfig_schema::Schema;
use mxconfig_schema::value::Value;
use mxconfig_schema::{Backend, Class};

/// Env var that names the only backend to try: `local` or `daemon`.
const BACKEND_ENV: &str = "MXCONFIG_BACKEND";

/// Where a namespace's values are read and written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Side {
    /// The layer files, read in this process.
    Local,
    /// The namespace's daemon.
    Daemon,
}

impl std::fmt::Display for Side {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Side::Local => "local files",
            Side::Daemon => "mxconfigd",
        })
    }
}

/// The backends to try for `namespace`, in order. `schema` may choose them.
pub(crate) fn sides(
    namespace: &str,
    schema: Option<&Schema>,
    serving: bool,
) -> Result<Vec<Side>, Error> {
    use Side::{Daemon, Local};
    let class = Class::of(namespace)?;
    // The daemon serves from files, whatever an app prefers.
    if serving {
        return Ok(vec![Local]);
    }
    // Clients reach a shared namespace only through its daemon.
    if class != Class::App {
        return Ok(vec![Daemon]);
    }
    match paths::env_var(BACKEND_ENV).as_deref() {
        Some("local") => return Ok(vec![Local]),
        Some("daemon") => return Ok(vec![Daemon]),
        _ => {}
    }
    Ok(match schema.and_then(Schema::backend) {
        Some(Backend::Daemon) => vec![Daemon, Local],
        Some(Backend::LocalOnly) => vec![Local],
        Some(Backend::DaemonOnly) => vec![Daemon],
        Some(Backend::Local) | None => vec![Local, Daemon],
    })
}

/// One edit in a [`Settings::write`] batch, which applies atomically.
#[derive(Debug, Clone, PartialEq)]
pub struct Edit {
    key: String,
    /// The new value, or `None` to unset the key.
    pub(crate) value: Option<Value>,
}

impl Edit {
    /// Set `key` to `value`.
    pub fn set(key: impl Into<String>, value: impl Into<Value>) -> Edit {
        Edit {
            key: key.into(),
            value: Some(value.into()),
        }
    }

    /// Unset `key`, so a lower layer or the default applies.
    pub fn clear(key: impl Into<String>) -> Edit {
        Edit {
            key: key.into(),
            value: None,
        }
    }

    /// The key it edits.
    pub fn key(&self) -> &str {
        &self.key
    }
}

/// A resolved asset: the file used for an id and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Asset {
    pub id: String,
    /// The file's path. For daemon assets, the path on the daemon's side.
    pub path: PathBuf,
    pub origin: Origin,
    /// For daemon assets, the file the daemon opened.
    pub(crate) served: Option<Served>,
}

impl Asset {
    /// The asset's bytes.
    pub fn read(&self) -> std::io::Result<Vec<u8>> {
        match &self.served {
            Some(served) => served.read(),
            None => fs::read(&self.path),
        }
    }

    /// The asset's text.
    pub fn text(&self) -> std::io::Result<String> {
        String::from_utf8(self.read()?)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}

/// What a handle holds, and whether it can write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum State {
    /// The namespace's real values. Writes can go through.
    Live,
    /// Real values, but schema version `holder` holds the namespace. Writes fail.
    ReadOnly { holder: u32 },
    /// The schema's defaults, because no daemon serves the namespace. Writes fail.
    Defaults(Unserved),
    /// No longer following the namespace. The last values stay. Writes fail.
    Detached(Detach),
}

impl std::fmt::Display for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            State::Live => f.write_str("it holds the namespace's values"),
            State::ReadOnly { holder } => {
                write!(f, "schema v{holder} holds the namespace, so it only reads")
            }
            State::Defaults(why) => write!(f, "it holds its schema defaults, as {why}"),
            State::Detached(cause) => write!(f, "it stopped following the namespace: {cause}"),
        }
    }
}

/// Why a shared namespace holds its schema defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Unserved {
    /// No transport carries the bus.
    NoBus,
    /// The bus is there, but mxconfigd isn't.
    NoDaemon,
    /// mxconfigd runs, but doesn't serve this namespace.
    NotServed,
    /// mxconfigd serves another schema version, which this client can't read.
    Version { served: Option<u32> },
}

impl std::fmt::Display for Unserved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unserved::NoBus => f.write_str("no transport carries its bus"),
            Unserved::NoDaemon => f.write_str("mxconfigd is not running"),
            Unserved::NotServed => f.write_str("mxconfigd does not serve it"),
            Unserved::Version { served: Some(v) } => {
                write!(
                    f,
                    "mxconfigd serves schema v{v}, which this client can't read"
                )
            }
            Unserved::Version { served: None } => {
                f.write_str("mxconfigd serves a schema version this client can't read")
            }
        }
    }
}

/// A handle on an open namespace. Clones share the same namespace.
#[derive(Clone)]
pub struct Settings {
    source: Source,
    key: Arc<Key>,
}

impl Settings {
    pub(crate) fn new(source: Source, key: Arc<Key>) -> Settings {
        Settings { source, key }
    }

    /// Read this handle's space.
    fn space<R>(&self, read: impl FnOnce(&Space) -> R) -> R {
        self.source.space(self.namespace(), read)
    }

    /// The source this handle belongs to.
    #[doc(hidden)]
    pub fn source(&self) -> &Source {
        &self.source
    }

    /// What this handle holds, and whether it can write. Check it when the handle opens.
    pub fn state(&self) -> State {
        self.space(Space::handle_state)
    }

    pub(crate) fn namespace(&self) -> &str {
        &self.key.namespace
    }

    /// The namespace's schema.
    pub fn describe(&self) -> Arc<Schema> {
        self.space(|space| Arc::clone(&space.schema))
    }

    /// The effective value for `key`, or `None` when it is unset.
    pub fn get(&self, key: &str) -> Option<Value> {
        self.space(|space| space.resolved.get(key).cloned())
    }

    /// Problems found while opening, migrating and reading.
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        self.space(Space::diagnostics)
    }

    /// Every value, its origin and the diagnostics, read together at one revision.
    pub fn resolved(&self) -> Resolved {
        self.space(Space::state)
    }

    /// Raised by each batch of changes. A daemon restart starts it over.
    pub fn revision(&self) -> u64 {
        self.space(Space::revision)
    }

    /// The file for asset `id`, or `None` if nothing sets it.
    pub fn asset(&self, id: &str) -> Option<Asset> {
        self.space(|space| space.asset(id))
    }
}

impl Settings {
    /// Fail unless `manifest` grants `access` to this namespace.
    pub fn with_manifest(self, manifest: &Manifest, access: Access) -> Result<Settings, Error> {
        manifest.check(self.namespace(), access)?;
        Ok(self)
    }

    /// Remove unknown keys and retired asset files, returning them.
    #[doc(hidden)]
    pub fn prune(&self) -> Result<Vec<String>, Error> {
        self.source.space_mut(self.namespace(), Space::prune)
    }
}

impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Settings")
            .field("namespace", &self.namespace())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_backend_picks_its_sides() {
        use Side::{Daemon, Local};
        let app = "org.example.app";
        let with = |backend: &str| {
            let text = format!("namespace = \"{app}\"\nversion = 1\nbackend = \"{backend}\"\n");
            sides(app, Some(&Schema::parse(&text).unwrap()), false).unwrap()
        };
        assert_eq!(with("local"), [Local, Daemon]);
        assert_eq!(with("daemon"), [Daemon, Local]);
        assert_eq!(with("local-only"), [Local]);
        assert_eq!(with("daemon-only"), [Daemon]);
        assert_eq!(sides(app, None, false).unwrap(), [Local, Daemon]);
        assert_eq!(
            sides(app, None, true).unwrap(),
            [Local],
            "a daemon reads files"
        );
    }
}
