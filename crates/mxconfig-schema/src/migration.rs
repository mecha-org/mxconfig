//! Migration edges, as a schema declares them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::schema::KeyType;

/// One migration operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Op {
    /// Move a value to another key.
    Rename { from: String, to: String },
    /// Convert a key's value to another type.
    Retype {
        key: String,
        from: KeyType,
        to: KeyType,
    },
    /// Replace a string value through `map`.
    Remap {
        key: String,
        map: BTreeMap<String, String>,
    },
    /// Move an asset file within the namespace directory.
    RenameAsset { from: String, to: String },
}

impl Op {
    /// The key paths or asset paths it changes.
    pub fn names(&self) -> Vec<&str> {
        match self {
            Op::Rename { from, to } | Op::RenameAsset { from, to } => vec![from, to],
            Op::Retype { key, .. } | Op::Remap { key, .. } => vec![key],
        }
    }

    /// The operation that undoes this one.
    #[must_use]
    pub fn inverse(&self) -> Op {
        match self {
            Op::Rename { from, to } => Op::Rename {
                from: to.clone(),
                to: from.clone(),
            },
            Op::Retype { key, from, to } => Op::Retype {
                key: key.clone(),
                from: to.clone(),
                to: from.clone(),
            },
            Op::Remap { key, map } => Op::Remap {
                key: key.clone(),
                map: map.iter().map(|(k, v)| (v.clone(), k.clone())).collect(),
            },
            Op::RenameAsset { from, to } => Op::RenameAsset {
                from: to.clone(),
                to: from.clone(),
            },
        }
    }
}

/// An edge from one schema version to a later one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Migration {
    pub from: u32,
    pub to: u32,
    #[serde(default)]
    pub ops: Vec<Op>,
}
