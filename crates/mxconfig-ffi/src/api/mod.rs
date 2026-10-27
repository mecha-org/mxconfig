//! The API bridged to Dart. Values are passed as TOML text.

use anyhow::{Context, Result};
use flutter_rust_bridge::frb;

use mxconfig::{Access, Manifest};
use mxconfig::{Edit, ErrorKind, Settings, Value};

use crate::convert::{mx_diagnostic, write};

pub mod source;

/// An open or write that failed: its message, and what it means for the caller.
#[frb(dart_code = "
  @override
  String toString() => 'MxconfigException: $message';
")]
#[derive(Debug)]
pub struct MxconfigException {
    pub message: String,
    /// Another schema version holds the namespace.
    pub version_conflict: bool,
    /// The other schema version in a version conflict, when it is known.
    pub other_version: Option<u32>,
    /// The handle stopped following its namespace.
    pub detached: bool,
    /// A write was sent but got no answer. It may have landed.
    pub unconfirmed: bool,
}

impl From<mxconfig::Error> for MxconfigException {
    fn from(e: mxconfig::Error) -> Self {
        MxconfigException {
            message: e.to_string(),
            version_conflict: e.kind() == ErrorKind::VersionConflict,
            other_version: e.other_version(),
            detached: e.kind() == ErrorKind::Detached,
            unconfirmed: e.kind() == ErrorKind::Unconfirmed,
        }
    }
}

impl From<anyhow::Error> for MxconfigException {
    fn from(e: anyhow::Error) -> Self {
        MxconfigException {
            message: format!("{e:#}"),
            version_conflict: false,
            other_version: None,
            detached: false,
            unconfirmed: false,
        }
    }
}

/// A loaded namespace.
#[derive(Debug)]
pub struct MxSettings {
    pub(crate) inner: Settings,
}

impl MxSettings {
    /// Fail unless `manifest_toml` grants the access.
    #[frb(sync)]
    pub fn authorize(&self, manifest_toml: String, write: bool) -> Result<(), MxconfigException> {
        let manifest = Manifest::parse(&manifest_toml).context("parsing the manifest")?;
        let access = if write {
            Access::ReadWrite
        } else {
            Access::Read
        };
        self.inner.clone().with_manifest(&manifest, access)?;
        Ok(())
    }

    #[frb(sync)]
    pub fn namespace(&self) -> String {
        self.inner.describe().namespace().to_string()
    }

    /// Raised by each batch of changes. A daemon restart starts it over.
    #[frb(sync)]
    pub fn revision(&self) -> u64 {
        self.inner.revision()
    }

    /// The value of `key`, or `null` when it is unset.
    #[frb(sync)]
    pub fn get_string(&self, key: String) -> Option<String> {
        mxconfig::value::to_text(&self.inner.get(&key)?)
    }

    #[frb(sync)]
    pub fn get_int(&self, key: String) -> Option<i64> {
        self.inner.get(&key)?.as_integer()
    }

    #[frb(sync)]
    pub fn get_double(&self, key: String) -> Option<f64> {
        mxconfig::value::as_f64(&self.inner.get(&key)?)
    }

    #[frb(sync)]
    pub fn get_bool(&self, key: String) -> Option<bool> {
        self.inner.get(&key)?.as_bool()
    }

    /// The value as TOML text. Used for arrays and maps.
    #[frb(sync)]
    pub fn get_toml(&self, key: String) -> Option<String> {
        self.inner.get(&key).map(|value| value.to_string())
    }

    #[frb(sync)]
    pub fn set_string(&self, key: String, value: String) -> Result<u64, MxconfigException> {
        write(&self.inner, Edit::set(key, Value::String(value)))
    }

    #[frb(sync)]
    pub fn set_int(&self, key: String, value: i64) -> Result<u64, MxconfigException> {
        write(&self.inner, Edit::set(key, Value::Integer(value)))
    }

    #[frb(sync)]
    pub fn set_double(&self, key: String, value: f64) -> Result<u64, MxconfigException> {
        write(&self.inner, Edit::set(key, Value::Float(value)))
    }

    #[frb(sync)]
    pub fn set_bool(&self, key: String, value: bool) -> Result<u64, MxconfigException> {
        write(&self.inner, Edit::set(key, Value::Boolean(value)))
    }

    /// Write a TOML value such as `35` or `[1, 2]` to `key`.
    #[frb(sync)]
    pub fn set_toml(&self, key: String, value: String) -> Result<u64, MxconfigException> {
        let value = mxconfig::value::from_toml_str(&value).map_err(mxconfig::Error::from)?;
        write(&self.inner, Edit::set(key, value))
    }

    /// Unset `key`, so a lower layer or the default applies.
    #[frb(sync)]
    pub fn remove(&self, key: String) -> Result<u64, MxconfigException> {
        write(&self.inner, Edit::remove(key))
    }

    /// The bytes of asset `id`, or `null` when nothing sets it.
    #[frb(sync)]
    pub fn asset(&self, id: String) -> Option<Vec<u8>> {
        self.inner.asset(&id)?.read().ok()
    }

    /// Write the user's copy of asset `id`. Returns the write's ticket.
    #[frb(sync)]
    pub fn set_asset(&self, id: String, bytes: Vec<u8>) -> Result<u64, MxconfigException> {
        Ok(self.inner.write_asset(&id, &bytes)?.id().into())
    }

    /// Copy the file at `path` as the user's copy of asset `id`.
    #[frb(sync)]
    pub fn set_asset_file(&self, id: String, path: String) -> Result<u64, MxconfigException> {
        let file = std::fs::File::open(&path).with_context(|| format!("opening {path}"))?;
        Ok(self.inner.write_asset_from(&id, file.into())?.id().into())
    }

    /// Remove the user's copy of asset `id`. Returns the write's ticket.
    #[frb(sync)]
    pub fn remove_asset(&self, id: String) -> Result<u64, MxconfigException> {
        Ok(self.inner.clear_asset(&id)?.id().into())
    }

    /// Problems found while opening, migrating and reading.
    #[frb(sync)]
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        self.inner.diagnostics().iter().map(mx_diagnostic).collect()
    }

    /// The schema, for settings UIs.
    #[frb(sync)]
    pub fn describe(&self) -> Describe {
        let schema = self.inner.describe();
        Describe {
            namespace: schema.namespace().to_string(),
            class_name: schema.class().to_string(),
            version: schema.version(),
            groups: schema
                .groups()
                .iter()
                .map(|g| GroupInfo {
                    id: g.id.clone(),
                    label: g.label.clone(),
                    order: g.order,
                })
                .collect(),
            keys: schema
                .keys()
                .iter()
                .map(|ks| KeyInfo {
                    key: ks.path.clone(),
                    group: ks.group.clone(),
                    ty: ks.ty.to_string(),
                    values: match &ks.ty {
                        mxconfig::KeyType::Enum(values) => values.clone(),
                        _ => Vec::new(),
                    },
                    default_toml: ks.default.as_ref().map(ToString::to_string),
                    label: ks.label.clone(),
                    description: ks.description.clone(),
                    order: ks.order,
                    min: ks.min,
                    max: ks.max,
                    max_length: ks.max_length.map(u64::cast_signed),
                    env: ks.env.clone(),
                    managed: ks.managed,
                })
                .collect(),
        }
    }

    /// Whether the values come from the daemon rather than this process's own reads.
    #[frb(sync)]
    pub fn from_daemon(&self) -> bool {
        self.inner.from_daemon()
    }

    /// Whether a shared namespace still holds its schema defaults.
    #[frb(sync)]
    pub fn on_defaults(&self) -> bool {
        self.inner.on_defaults()
    }

    /// The version holding the namespace, when it isn't this handle's. The handle then only reads.
    #[frb(sync)]
    pub fn other_version(&self) -> Option<u32> {
        self.inner.other_version()
    }
}

/// One effective value that changed.
#[frb(dart_code = "
  /// Whether the key became unset.
  bool get isUnset => value == null;
")]
#[derive(Debug)]
pub struct Change {
    pub namespace: String,
    pub key: String,
    /// The new value as TOML text, or `null` when the key became unset.
    pub value: Option<String>,
    /// The new value as each Dart type, if it is one.
    pub string_value: Option<String>,
    pub int_value: Option<i64>,
    pub double_value: Option<f64>,
    pub bool_value: Option<bool>,
}

/// A problem with a key or file.
#[frb(dart_code = "
  bool get isError => severity == 'error';
")]
#[derive(Debug)]
pub struct Diagnostic {
    /// `error` or `warning`.
    pub severity: String,
    pub key: Option<String>,
    pub source: Option<String>,
    pub message: String,
}

/// A schema, for settings UIs.
#[frb(dart_code = "
  /// The keys of [group], in display order.
  List<KeyInfo> keysOf(String group) =>
      keys.where((key) => key.group == group).toList();
")]
#[derive(Debug)]
pub struct Describe {
    pub namespace: String,
    pub class_name: String,
    pub version: u32,
    pub groups: Vec<GroupInfo>,
    pub keys: Vec<KeyInfo>,
}

/// One group, in display order.
#[derive(Debug)]
pub struct GroupInfo {
    pub id: String,
    pub label: String,
    pub order: i64,
}

/// One key, with what a settings UI shows.
#[derive(Debug)]
pub struct KeyInfo {
    pub key: String,
    pub group: String,
    /// The key's type name, e.g. `string` or `enum`.
    pub ty: String,
    /// An enum key's values. Empty for other types.
    pub values: Vec<String>,
    /// The default as TOML text, or `null` when there is none.
    pub default_toml: Option<String>,
    pub label: String,
    pub description: String,
    pub order: i64,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub max_length: Option<i64>,
    /// The env var that overrides the key, if any.
    pub env: Option<String>,
    /// Only vendor and admin files set it. Writes to it are refused.
    pub managed: bool,
}
