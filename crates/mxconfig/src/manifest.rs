//! The client manifest: what a client declares it may read or write.

use std::path::Path;

use crate::error::{Diagnostic, Error, Kind};
use serde::Deserialize;

/// The access a manifest entry grants, ordered from weakest to strongest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Access {
    Read,
    ReadWrite,
}

impl std::fmt::Display for Access {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Access::Read => "read",
            Access::ReadWrite => "read-write",
        })
    }
}

/// One namespace a client declares.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct Entry {
    namespace: String,
    access: Access,
    /// A grant for development that must not be shipped.
    #[serde(default)]
    temporary: bool,
}

/// A manifest as written, before its checks.
#[derive(Deserialize)]
struct Raw {
    client: String,
    #[serde(default)]
    settings: Vec<Entry>,
}

/// A client's declared settings access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub client: String,
    entries: Vec<Entry>,
}

impl Manifest {
    /// Parse a manifest from TOML text.
    pub fn parse(src: &str) -> Result<Manifest, Error> {
        let raw: Raw = toml::from_str(src).map_err(|e| Kind::Manifest(e.to_string()))?;
        let manifest = Manifest {
            client: raw.client,
            entries: raw.settings,
        };
        // At most one lasting and one temporary grant per namespace.
        let mut seen = std::collections::BTreeSet::new();
        for entry in &manifest.entries {
            if !seen.insert((entry.namespace.as_str(), entry.temporary)) {
                return Err(Kind::Manifest(format!(
                    "`{}` declares `{}` twice",
                    manifest.client, entry.namespace
                ))
                .into());
            }
        }
        Ok(manifest)
    }

    /// Read and parse the manifest at `path`.
    #[doc(hidden)]
    pub fn from_file(path: &Path) -> Result<Manifest, Error> {
        let src = std::fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
        Manifest::parse(&src)
    }

    /// Errors for temporary grants, which must not ship.
    #[doc(hidden)]
    pub fn validate(&self) -> Vec<Diagnostic> {
        self.entries
            .iter()
            .filter(|entry| entry.temporary)
            .map(|entry| {
                Diagnostic::error(format!(
                    "temporary {} grant to `{}` must not ship",
                    entry.access, entry.namespace
                ))
                .with_source("manifest")
            })
            .collect()
    }

    /// The strongest access declared for `namespace`.
    fn access(&self, namespace: &str) -> Option<Access> {
        self.entries
            .iter()
            .filter(|e| e.namespace == namespace)
            .map(|e| e.access)
            .max()
    }

    /// Fail unless the manifest grants `wanted` access to `namespace`.
    #[doc(hidden)]
    pub fn check(&self, namespace: &str, wanted: Access) -> Result<(), Error> {
        let declared = match self.access(namespace) {
            Some(access) if access >= wanted => return Ok(()),
            Some(_) => "read-only",
            None => "no",
        };
        let client = &self.client;
        let message = format!("`{client}` declares {declared} access to `{namespace}`");
        Err(Kind::Access(message).into())
    }
}
