use std::path::PathBuf;
use std::sync::Arc;

use thiserror::Error;

use crate::host::protocol::Failure;
use crate::source::Detach;
use crate::transport::Bus;

pub use mxconfig_schema::{Diagnostic, Severity};

/// An error that stops an operation. Per-key problems are diagnostics.
#[derive(Debug, Clone, Error)]
#[error(transparent)]
pub struct Error(Box<Kind>);

/// What went wrong.
#[derive(Debug, Clone, Error)]
pub(crate) enum Kind {
    /// A schema, namespace id or value that could not be read.
    #[error(transparent)]
    Read(mxconfig_schema::Error),

    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: Arc<std::io::Error>,
    },

    #[error("no schema installed for `{0}`")]
    NotInstalled(String),

    #[error("invalid manifest: {0}")]
    Manifest(String),

    #[error("migration failed: {0}")]
    Migration(String),

    #[error("`{key}` is not a key of `{namespace}`")]
    UnknownKey { namespace: String, key: String },

    #[error("`{id}` is not an asset of `{namespace}`")]
    UnknownAsset { namespace: String, id: String },

    /// A value the schema rejects for `key`.
    #[error("`{key}` {reason}")]
    Invalid { key: String, reason: String },

    #[error("{0}")]
    Access(String),

    #[error("`{0}` is managed, so only vendor and admin files set it")]
    Managed(String),

    #[error("`{0}` has no user directory to write to")]
    NoUserDir(String),

    #[error("the runtime cannot reach {0}")]
    Unreachable(Bus),

    /// The daemon refused or failed, as `Failure` says.
    #[error("daemon: {1}")]
    Daemon(Failure, String),

    /// Both backends failed: the first one tried, then the other.
    #[error("{first}; the other backend failed too: {then}")]
    Fallback { first: Error, then: Error },

    /// Another schema version holds the namespace, or its file has moved on.
    #[error("{why}")]
    VersionConflict { why: String, other: Option<u32> },

    #[error("`{0}` is detached: {1}")]
    Detached(String, Detach),

    #[error("{0} gave no answer, so the write may or may not have landed")]
    Unconfirmed(Bus),

    /// A file needs migrating, but its lock can't be held.
    #[error("{0}")]
    Unlocked(String),
}

impl Error {
    /// An I/O error with the path it happened at.
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Kind::Io {
            path: path.into(),
            source: Arc::new(source),
        }
        .into()
    }

    /// A value the schema rejects for `key`.
    #[doc(hidden)]
    pub fn invalid(key: impl Into<String>, reason: impl Into<String>) -> Self {
        Kind::Invalid {
            key: key.into(),
            reason: reason.into(),
        }
        .into()
    }

    /// The other schema version in a version conflict, when it is known.
    pub fn other_version(&self) -> Option<u32> {
        match self.cause() {
            Kind::VersionConflict { other, .. } => *other,
            _ => None,
        }
    }

    /// What went wrong, from the first backend tried if both failed.
    pub(crate) fn cause(&self) -> &Kind {
        match &*self.0 {
            Kind::Fallback { first, .. } => first.cause(),
            kind => kind,
        }
    }

    /// Whether the other backend might serve what this one could not.
    pub(crate) fn can_fall_back(&self) -> bool {
        matches!(
            self.cause(),
            Kind::Unreachable(_)
                | Kind::Daemon(Failure::Unknown, _)
                | Kind::NotInstalled(_)
                | Kind::Io { .. }
        )
    }

    /// Whether a daemon couldn't serve the request, rather than refused it.
    pub(crate) fn unavailable(&self) -> bool {
        matches!(
            self.cause(),
            Kind::Unreachable(_) | Kind::Daemon(Failure::Unknown, _)
        )
    }

    /// The error for `what` being at schema v`theirs` while this one is v`own`.
    pub(crate) fn version_conflict(what: impl std::fmt::Display, theirs: u32, own: u32) -> Error {
        let why = format!("{what} is at schema v{theirs}, and this is v{own}");
        let other = Some(theirs);
        Kind::VersionConflict { why, other }.into()
    }

    /// The D-Bus failure a daemon answers with, the inverse of a client's reading.
    #[doc(hidden)]
    pub fn failure(&self) -> Failure {
        match self.kind() {
            ErrorKind::Denied => Failure::Denied,
            ErrorKind::Invalid => Failure::Invalid,
            ErrorKind::Unknown => Failure::Unknown,
            ErrorKind::VersionConflict | ErrorKind::Detached => Failure::Version,
            _ => Failure::Other,
        }
    }

    /// What kind of error this is, for deciding what to do about it.
    pub fn kind(&self) -> ErrorKind {
        match self.cause() {
            Kind::Access(_) | Kind::Managed(_) => ErrorKind::Denied,
            Kind::Invalid { .. } | Kind::UnknownKey { .. } | Kind::Daemon(Failure::Invalid, _) => {
                ErrorKind::Invalid
            }
            Kind::Read(e) if e.is_value() => ErrorKind::Invalid,
            Kind::NotInstalled(_)
            | Kind::Daemon(Failure::Unknown, _)
            | Kind::UnknownAsset { .. } => ErrorKind::Unknown,
            &Kind::Unreachable(bus) => ErrorKind::Unreachable(bus),
            Kind::VersionConflict { .. } => ErrorKind::VersionConflict,
            Kind::Detached(..) => ErrorKind::Detached,
            Kind::Unconfirmed(_) => ErrorKind::Unconfirmed,
            _ => ErrorKind::Other,
        }
    }
}

/// What kind of [`Error`](struct@Error) happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    /// Access was refused, by policy, polkit or a managed key.
    Denied,
    /// A key, value or TOML text was rejected.
    Invalid,
    /// No schema, asset or served namespace by that name.
    Unknown,
    /// The runtime could not reach this bus.
    Unreachable(Bus),
    /// Another schema version holds the namespace, so it can't be opened now.
    VersionConflict,
    /// The handle stopped following its namespace. [`Detach`] says why.
    Detached,
    /// A write was sent but got no answer. It may have landed.
    Unconfirmed,
    /// Anything else, such as a file that can't be read.
    Other,
}

impl From<mxconfig_schema::Error> for Error {
    fn from(e: mxconfig_schema::Error) -> Self {
        Kind::Read(e).into()
    }
}

impl From<Kind> for Error {
    fn from(kind: Kind) -> Self {
        Error(Box::new(kind))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::failure;

    /// What a daemon answers with reads back as the same kind on the client.
    #[test]
    fn a_failure_reads_back_as_its_kind() {
        let errors: [Error; 5] = [
            Kind::Access("no".into()).into(),
            Error::invalid("key", "is bad"),
            Kind::NotInstalled("org.example.x".into()).into(),
            Kind::VersionConflict {
                why: "v2 holds it".into(),
                other: None,
            }
            .into(),
            Kind::Daemon(Failure::Other, "broke".into()).into(),
        ];
        for e in errors {
            let read = failure(Bus::Session, e.failure().name(), e.to_string(), false);
            assert_eq!(read.kind(), e.kind(), "{e}");
        }
    }
}
