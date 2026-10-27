//! Resolution: schema defaults, then each layer in order, then env vars.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::error::Diagnostic;
use crate::paths::{self, Layer, LayerKind};
use mxconfig_schema::value::{self, Value};
use mxconfig_schema::{KeySchema, KeyType, Schema};

/// Where an effective value came from.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Origin {
    /// The schema's default.
    Schema,
    /// A file in a layer.
    File { kind: LayerKind, path: PathBuf },
    /// An env var declared in the schema.
    Env { var: String },
    /// mxconfigd, from its own layers.
    Daemon,
}

/// Where a value came from, as one short phrase.
impl std::fmt::Display for Origin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Origin::Schema => f.write_str("schema"),
            Origin::File { kind, path } => write!(f, "{kind} {}", path.display()),
            Origin::Env { var } => write!(f, "env ${var}"),
            Origin::Daemon => f.write_str("daemon"),
        }
    }
}

/// A namespace's effective values, their origins and diagnostics, read at one revision.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub(crate) entries: BTreeMap<String, Entry>,
    pub(crate) diagnostics: Vec<Diagnostic>,
    pub(crate) revision: u64,
}

/// One effective value and where it came from.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Entry {
    pub value: Value,
    pub origin: Origin,
}

impl Entry {
    fn daemon(value: Value) -> Entry {
        Entry {
            value,
            origin: Origin::Daemon,
        }
    }
}

impl Resolved {
    /// The effective value for `key`, or `None` when it is unset.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries.get(key).map(|entry| &entry.value)
    }

    pub fn origin(&self, key: &str) -> Option<&Origin> {
        self.entries.get(key).map(|entry| &entry.origin)
    }

    /// The revision these values were read at.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Problems found while opening, migrating and reading.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.entries.iter().map(|(key, entry)| (key, &entry.value))
    }

    /// Use `value` for `key` if valid, else record a diagnostic.
    fn accept(&mut self, key: &KeySchema, value: Value, origin: Origin, source: &str) {
        let reasons = key.check(&value);
        if reasons.is_empty() {
            self.entries
                .insert(key.path.clone(), Entry { value, origin });
            return;
        }
        let report = |reason| {
            let error = Diagnostic::error(format!("`{}` {reason}", key.path));
            error.with_key(&key.path).with_source(source)
        };
        self.diagnostics.extend(reasons.into_iter().map(report));
    }

    /// Values resolved by the daemon.
    pub(crate) fn from_daemon(values: BTreeMap<String, Value>) -> Resolved {
        let entries = values
            .into_iter()
            .map(|(key, value)| (key, Entry::daemon(value)));
        Resolved {
            entries: entries.collect(),
            ..Resolved::empty()
        }
    }

    /// No values or diagnostics, at revision 0.
    pub(crate) fn empty() -> Resolved {
        Resolved {
            entries: BTreeMap::new(),
            diagnostics: Vec::new(),
            revision: 0,
        }
    }
}

#[cfg(test)]
thread_local! {
    /// How many times this thread resolved a namespace. For tests.
    pub(crate) static READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The schema defaults alone, with no layers and no env vars.
pub(crate) fn defaults(schema: &Schema) -> Resolved {
    resolve_with(schema, &[], |_| None)
}

/// Resolve a namespace from layers, lowest precedence first, then env vars.
pub(crate) fn resolve(schema: &Schema, layers: &[Layer]) -> Resolved {
    #[cfg(test)]
    READS.with(|reads| reads.set(reads.get() + 1));
    resolve_with(schema, layers, paths::env_var)
}

/// Parse an env var as `ty`. Scalars are bare, arrays and maps are TOML.
fn from_env_str(text: &str, ty: &KeyType) -> Option<Value> {
    let trimmed = text.trim();
    Some(match ty {
        KeyType::String | KeyType::Enum(_) => Value::String(text.to_string()),
        KeyType::Integer => Value::Integer(trimmed.parse().ok()?),
        KeyType::Float => Value::Float(trimmed.parse().ok()?),
        KeyType::Bool => Value::Boolean(match trimmed {
            "true" | "1" => true,
            "false" | "0" => false,
            _ => return None,
        }),
        KeyType::Datetime => Value::Datetime(trimmed.parse().ok()?),
        KeyType::Array(_) | KeyType::Map(_) => value::from_toml_str(trimmed).ok()?,
    })
}

/// [`resolve`], with env vars read through `env`.
fn resolve_with(
    schema: &Schema,
    layers: &[Layer],
    env: impl Fn(&str) -> Option<String>,
) -> Resolved {
    let mut out = Resolved::empty();

    for key in schema.keys() {
        if let Some(value) = key.default.clone() {
            let origin = Origin::Schema;
            out.entries
                .insert(key.path.clone(), Entry { value, origin });
        }
    }

    // Read every file first, so read errors come before key errors.
    let files: Vec<_> = paths::sources(layers, schema)
        .into_iter()
        .filter_map(|(kind, path)| {
            let doc = paths::read_doc(&path, kind == LayerKind::User, &mut out.diagnostics)?;
            Some((kind, doc, path))
        })
        .collect();
    let renames = schema.key_renames();
    for (kind, doc, file) in files {
        let source = file.display().to_string();
        let mut entries = Vec::new();
        collect(&doc, schema, "", &mut entries);

        for (path, key, raw) in entries {
            let Some(key) = key else {
                let note = match renames.iter().find(|(old, _)| *old == path) {
                    Some((_, now)) => format!("`{path}` is now `{now}`"),
                    None if schema.retired().keys.contains(&path) => format!("`{path}` is retired"),
                    None => format!("unknown key `{path}`"),
                };
                let note = Diagnostic::warning(note).with_key(path);
                out.diagnostics.push(note.with_source(&source));
                continue;
            };
            if key.managed && kind == LayerKind::User {
                let note = format!("`{path}` is managed, so the user's value is ignored");
                let note = Diagnostic::warning(note)
                    .with_key(path)
                    .with_source(&source);
                out.diagnostics.push(note);
                continue;
            }
            let origin = Origin::File {
                kind,
                path: file.clone(),
            };
            out.accept(key, raw.clone(), origin, &source);
        }
    }

    // Migrations move only the user's files, so a system file may still use an old asset path.
    let moved = schema.asset_renames();
    for layer in layers.iter().filter(|layer| layer.kind != LayerKind::User) {
        let dir = layer.home(schema).dir;
        for (old, now) in &moved {
            let file = dir.join(old);
            if file.is_file() {
                let note = Diagnostic::warning(format!("asset file `{old}` is now `{now}`"));
                out.diagnostics
                    .push(note.with_source(file.display().to_string()));
            }
        }
    }

    // Env vars take precedence over every layer.
    for key in schema.keys() {
        let Some(var) = key.env_var() else { continue };
        let Some(text) = env(var) else { continue };
        let source = format!("env ${var}");
        match from_env_str(&text, &key.ty) {
            Some(value) => {
                let origin = Origin::Env { var: var.into() };
                out.accept(key, value, origin, &source);
            }
            None => out.diagnostics.push(
                Diagnostic::error(format!(
                    "`{}` expects {} but ${var} is `{text}`",
                    key.path, key.ty
                ))
                .with_key(&key.path)
                .with_source(source),
            ),
        }
    }

    out
}

/// Collect a document's keys: path, schema key if declared, and value.
pub(crate) fn collect<'a>(
    doc: &'a toml::Value,
    schema: &'a Schema,
    prefix: &str,
    out: &mut Vec<(String, Option<&'a KeySchema>, &'a toml::Value)>,
) {
    let toml::Value::Table(table) = doc else {
        return;
    };
    for (name, value) in table {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}.{name}")
        };
        match schema.key(&path) {
            None if value.is_table() => collect(value, schema, &path, out),
            key => out.push((path, key, value)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = r#"
namespace = "org.mechanix.demo"
version = 1

[groups.output]
label = "Output"

[keys.output.scale]
type = "float"
default = 1.0
min = 0.5
max = 3.0
env = "DEMO_SCALE"

[keys.output.theme]
type = "string"
default = "default"
env = "DEMO_THEME"

[keys.verbose]
type = "bool"
default = false
env = "DEMO_VERBOSE"
"#;

    fn resolve_env(schema: &str, vars: &[(&str, &str)]) -> Resolved {
        let schema = Schema::parse(schema).unwrap();
        resolve_with(&schema, &[], |var| {
            let found = vars.iter().find(|(name, _)| *name == var);
            found.map(|(_, value)| (*value).to_string())
        })
    }

    #[test]
    fn a_declared_env_var_overrides_the_key() {
        let resolved = resolve_env(
            SCHEMA,
            &[
                ("DEMO_SCALE", "1.5"),
                ("DEMO_THEME", "Adwaita"),
                ("DEMO_VERBOSE", "1"),
            ],
        );
        assert_eq!(resolved.get("output.scale"), Some(&Value::Float(1.5)));
        assert_eq!(
            resolved.get("output.theme"),
            Some(&Value::String("Adwaita".into()))
        );
        assert_eq!(resolved.get("verbose"), Some(&Value::Boolean(true)));
        assert_eq!(
            resolved.origin("output.scale"),
            Some(&Origin::Env {
                var: "DEMO_SCALE".into()
            })
        );
        assert!(
            resolved.diagnostics.is_empty(),
            "{:?}",
            resolved.diagnostics
        );
    }

    #[test]
    fn a_bad_env_value_is_reported_and_falls_back() {
        let resolved = resolve_env(SCHEMA, &[("DEMO_SCALE", "big"), ("DEMO_VERBOSE", "9")]);
        assert_eq!(resolved.get("output.scale"), Some(&Value::Float(1.0)));
        assert_eq!(resolved.get("verbose"), Some(&Value::Boolean(false)));
        assert_eq!(resolved.diagnostics.len(), 2);

        // Bounds are checked the same way as for file values.
        let resolved = resolve_env(SCHEMA, &[("DEMO_SCALE", "9")]);
        assert_eq!(resolved.get("output.scale"), Some(&Value::Float(1.0)));
        assert!(resolved.diagnostics[0].message().contains("above max"));
    }

    #[test]
    fn a_managed_key_ignores_env() {
        let managed = SCHEMA.replace(
            "env = \"DEMO_SCALE\"",
            "env = \"DEMO_SCALE\"\nmanaged = true",
        );
        let resolved = resolve_env(&managed, &[("DEMO_SCALE", "2")]);
        assert_eq!(resolved.get("output.scale"), Some(&Value::Float(1.0)));
    }
}
