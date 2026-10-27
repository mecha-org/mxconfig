//! Namespace schemas: keys, groups, assets and their UI text.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::class::Class;
use crate::error::{Error, Kind};
use crate::migration::{Migration, Op};
use crate::value::Value;

/// Where an app's own namespace is read, and whether the other may stand in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Backend {
    /// Local files first, then the daemon.
    Local,
    /// The daemon first, then local files.
    Daemon,
    /// Local files only.
    LocalOnly,
    /// The daemon only.
    DaemonOnly,
}

/// A key's declared type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeyType {
    String,
    Integer,
    Float,
    Bool,
    Datetime,
    /// An array, with an optional element type.
    Array(Option<Box<KeyType>>),
    /// A map, with an optional value type.
    Map(Option<Box<KeyType>>),
    /// A string from a fixed set, generated as an enum.
    Enum(Vec<String>),
}

impl KeyType {
    /// Whether `value` has this type. Elements are checked separately.
    pub(crate) fn matches(&self, value: &Value) -> bool {
        match (self, value) {
            (KeyType::Enum(values), Value::String(s)) => values.contains(s),
            (KeyType::String, Value::String(_))
            | (KeyType::Integer, Value::Integer(_))
            | (KeyType::Float, Value::Float(_))
            | (KeyType::Bool, Value::Boolean(_))
            | (KeyType::Datetime, Value::Datetime(_))
            | (KeyType::Array(_), Value::Array(_))
            | (KeyType::Map(_), Value::Table(_)) => true,
            _ => false,
        }
    }
}

impl fmt::Display for KeyType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            KeyType::String => "string",
            KeyType::Integer => "integer",
            KeyType::Float => "float",
            KeyType::Bool => "bool",
            KeyType::Datetime => "datetime",
            KeyType::Array(_) => "array",
            KeyType::Map(_) => "map",
            KeyType::Enum(_) => "enum",
        })
    }
}

/// One declared key.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct KeySchema {
    /// The dotted path, e.g. `colors.accent`.
    pub path: String,
    /// The last path segment.
    pub id: String,
    /// The key's group, or empty when ungrouped.
    pub group: String,
    pub ty: KeyType,
    /// `None` means the key is unset by default.
    pub default: Option<Value>,
    pub label: String,
    pub description: String,
    pub order: i64,
    /// Bounds, used for validation and as UI hints.
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub max_length: Option<u64>,
    /// An env var that overrides every layer.
    pub env: Option<String>,
    /// Only vendor and admin values apply. Users can't set it.
    pub managed: bool,
}

impl KeySchema {
    /// Its env var, unless it is managed, which ignores env.
    pub fn env_var(&self) -> Option<&str> {
        self.env.as_deref().filter(|_| !self.managed)
    }
}

/// A key as written in the schema file.
#[derive(Deserialize)]
struct RawKey {
    #[serde(rename = "type")]
    ty: String,
    #[serde(default)]
    values: Vec<String>,
    default: Option<Value>,
    #[serde(default)]
    label: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    order: i64,
    min: Option<f64>,
    max: Option<f64>,
    max_length: Option<u64>,
    element: Option<String>,
    #[serde(default)]
    element_values: Vec<String>,
    #[serde(default)]
    managed: bool,
    env: Option<String>,
}

impl KeySchema {
    /// Read key `id` of `group` from its table. `group` is empty if ungrouped.
    fn read(value: Value, id: &str, group: &str) -> Result<KeySchema, Error> {
        let path = match group {
            "" => id.to_string(),
            group => format!("{group}.{id}"),
        };
        let bad = |message: String| invalid(format!("key `{path}`: {message}"));
        let raw: RawKey = value.try_into().map_err(|e| bad(e.to_string()))?;
        let element = raw
            .element
            .map(|name| key_type(&name, raw.element_values).map(Box::new))
            .transpose()
            .map_err(bad)?;
        // Only arrays and maps have an element type.
        let ty = match key_type(&raw.ty, raw.values).map_err(bad)? {
            KeyType::Array(_) => KeyType::Array(element),
            KeyType::Map(_) => KeyType::Map(element),
            ty => ty,
        };
        Ok(KeySchema {
            id: id.to_string(),
            group: group.to_string(),
            path,
            ty,
            default: raw.default,
            label: raw.label,
            description: raw.description,
            order: raw.order,
            min: raw.min,
            max: raw.max,
            max_length: raw.max_length,
            env: raw.env,
            managed: raw.managed,
        })
    }
}

/// The type named `name`. `values` are used by enums.
fn key_type(name: &str, values: Vec<String>) -> Result<KeyType, String> {
    Ok(match name {
        "string" => KeyType::String,
        "integer" => KeyType::Integer,
        "float" => KeyType::Float,
        "bool" => KeyType::Bool,
        "datetime" => KeyType::Datetime,
        "array" => KeyType::Array(None),
        "map" => KeyType::Map(None),
        "enum" => KeyType::Enum(values),
        _ => return Err(format!("unknown type `{name}`")),
    })
}

/// A group of keys, as shown in a settings UI.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[non_exhaustive]
pub struct Group {
    /// The group id, from the `groups` table.
    #[serde(skip)]
    pub id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub order: i64,
}

/// A declared asset: a file in the namespace directory.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[non_exhaustive]
pub struct AssetSchema {
    /// The asset id, from the `assets` table.
    #[serde(skip)]
    pub id: String,
    /// A relative path in the namespace directory, without `..`.
    pub path: String,
    /// An env var with an absolute path that overrides the asset.
    pub env: Option<String>,
    /// Only vendor and admin copies apply. Users can't set it.
    #[serde(default)]
    pub managed: bool,
}

impl AssetSchema {
    /// Its env var, unless it is managed, which ignores env.
    pub fn env_var(&self) -> Option<&str> {
        self.env.as_deref().filter(|_| !self.managed)
    }
}

/// A namespace's schema, built by [`Schema::parse`].
#[derive(Debug, Clone, PartialEq)]
pub struct Schema {
    namespace: String,
    /// An app namespace's directory name. Defaults to the namespace id.
    dir: Option<String>,
    backend: Option<Backend>,
    version: u32,
    class: Class,
    /// In display order.
    groups: Vec<Group>,
    /// In display order: grouped keys first, then by `order` and id.
    keys: Vec<KeySchema>,
    assets: Vec<AssetSchema>,
    migrations: Vec<Migration>,
    retired: Retired,
    /// The text it was parsed from.
    source: String,
}

/// Names a schema no longer uses. They may never be declared again.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Retired {
    /// Dotted key paths.
    #[serde(default)]
    pub keys: Vec<String>,
    /// Asset file paths, relative to the namespace directory.
    #[serde(default)]
    pub assets: Vec<String>,
}

/// The schema file as parsed, before conversion.
#[derive(Deserialize)]
struct RawSchema {
    namespace: String,
    dir: Option<String>,
    backend: Option<Backend>,
    version: u32,
    #[serde(default)]
    assets: BTreeMap<String, AssetSchema>,
    #[serde(default)]
    groups: BTreeMap<String, Group>,
    #[serde(default)]
    keys: BTreeMap<String, Value>,
    #[serde(default, rename = "migration")]
    migrations: Vec<Migration>,
    #[serde(default)]
    retired: Retired,
}

impl Schema {
    pub fn parse(src: &str) -> Result<Schema, Error> {
        let raw: RawSchema = toml::from_str(src).map_err(|e| invalid(e.to_string()))?;
        let class = Class::of(&raw.namespace)?;
        let shared = class != Class::App;
        if raw.version < 1 {
            return Err(invalid("`version` must be >= 1"));
        }
        let own = if raw.dir.is_some() {
            Some("dir")
        } else if raw.backend.is_some() {
            Some("backend")
        } else {
            None
        };
        if shared && let Some(field) = own {
            let namespace = &raw.namespace;
            let message =
                format!("`{field}` is for an app's own namespace; `{namespace}` is shared");
            return Err(invalid(message));
        }
        if let Some(dir) = &raw.dir
            && !one_name(dir)
        {
            return Err(invalid(format!("`dir` `{dir}` is not one directory name")));
        }

        let assets: Vec<AssetSchema> = raw
            .assets
            .into_iter()
            .map(|(id, asset)| AssetSchema { id, ..asset })
            .collect();
        if let Some(asset) = assets.iter().find(|asset| !inside(&asset.path)) {
            let (id, path) = (&asset.id, &asset.path);
            let message = format!("asset `{id}` has an invalid path `{path}`");
            return Err(invalid(message));
        }

        let mut groups: Vec<Group> = raw
            .groups
            .into_iter()
            .map(|(id, group)| Group { id, ..group })
            .collect();
        // A stable sort keeps ties in id order.
        groups.sort_by_key(|group| group.order);

        let keys = read_keys(raw.keys, &groups)?;
        check_env(&keys, &assets)?;
        check_retired(&raw.retired, &keys, &assets)?;

        Ok(Schema {
            namespace: raw.namespace,
            dir: raw.dir,
            backend: raw.backend,
            version: raw.version,
            class,
            groups,
            keys,
            assets,
            migrations: raw.migrations,
            retired: raw.retired,
            source: src.to_string(),
        })
    }

    /// Parse `src`, which must be `namespace`'s schema.
    pub fn parse_for(namespace: &str, src: &str) -> Result<Schema, Error> {
        let schema = Schema::parse(src)?;
        if schema.namespace() != namespace {
            let other = schema.namespace();
            return Err(invalid(format!("it is for `{other}`, not `{namespace}`")));
        }
        Ok(schema)
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// The text it was parsed from.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The directory name an app namespace uses in each layer.
    #[doc(hidden)]
    pub fn dir(&self) -> &str {
        self.dir.as_deref().unwrap_or(&self.namespace)
    }

    /// The backend an app asks for, if it chose one.
    #[doc(hidden)]
    pub fn backend(&self) -> Option<Backend> {
        self.backend
    }

    pub fn version(&self) -> u32 {
        self.version
    }

    /// Whether any key or asset is managed.
    pub fn has_managed(&self) -> bool {
        self.keys.iter().any(|key| key.managed) || self.assets.iter().any(|a| a.managed)
    }

    pub fn class(&self) -> Class {
        self.class
    }

    /// Groups in display order.
    pub fn groups(&self) -> &[Group] {
        &self.groups
    }

    /// Keys in display order: grouped keys first, then by `order` and id.
    pub fn keys(&self) -> &[KeySchema] {
        &self.keys
    }

    /// The keys of `group` in display order. `""` gives the ungrouped keys.
    pub fn in_group<'a>(&'a self, group: &'a str) -> impl Iterator<Item = &'a KeySchema> {
        self.keys.iter().filter(move |key| key.group == group)
    }

    /// The key at a dotted path, if declared.
    pub fn key(&self, path: &str) -> Option<&KeySchema> {
        self.keys.iter().find(|key| key.path == path)
    }

    pub fn assets(&self) -> &[AssetSchema] {
        &self.assets
    }

    /// The asset `id`, if declared.
    pub fn asset(&self, id: &str) -> Option<&AssetSchema> {
        self.assets.iter().find(|asset| asset.id == id)
    }

    /// The migration edges from `[[migration]]`.
    #[doc(hidden)]
    pub fn migrations(&self) -> &[Migration] {
        &self.migrations
    }

    /// The keys and asset paths it no longer uses.
    #[doc(hidden)]
    pub fn retired(&self) -> &Retired {
        &self.retired
    }

    /// Each renamed key's old path and its current one.
    #[doc(hidden)]
    pub fn key_renames(&self) -> Vec<(&str, &str)> {
        self.renames(|op| match op {
            Op::Rename { from, to } => Some((from, to)),
            _ => None,
        })
    }

    /// Each renamed asset's old path and its current one.
    #[doc(hidden)]
    pub fn asset_renames(&self) -> Vec<(&str, &str)> {
        self.renames(|op| match op {
            Op::RenameAsset { from, to } => Some((from, to)),
            _ => None,
        })
    }

    /// The renames `pick` finds, followed through every edge in version order.
    fn renames<'a>(
        &'a self,
        pick: impl Fn(&'a Op) -> Option<(&'a String, &'a String)>,
    ) -> Vec<(&'a str, &'a str)> {
        let mut edges: Vec<&Migration> = self.migrations.iter().collect();
        edges.sort_by_key(|edge| edge.from);
        let mut renames: Vec<(&str, &str)> = Vec::new();
        for (from, to) in edges.iter().flat_map(|edge| &edge.ops).filter_map(pick) {
            match renames.iter_mut().find(|(_, now)| now == from) {
                Some((_, now)) => *now = to,
                None => renames.push((from, to)),
            }
        }
        renames
    }

    /// Whether a file at `other`'s version reads the same as at this one, for every name declared here.
    pub fn reads_same(&self, other: &Schema) -> bool {
        let newer = if other.version > self.version {
            other
        } else {
            self
        };
        self.reads_through(other.version, &newer.migrations)
    }

    /// Whether a file at version `other` reads the same as at this one, judged by `edges`.
    #[doc(hidden)]
    pub fn reads_through(&self, other: u32, edges: &[Migration]) -> bool {
        let (mut at, to) = (self.version.min(other), self.version.max(other));
        let declared = |name: &str| {
            self.key(name).is_some() || self.assets.iter().any(|asset| asset.path == name)
        };
        while at < to {
            // Without a path between the versions, nothing can be said.
            let Some(edge) = next_edge(edges, at, to) else {
                return false;
            };
            if edge.ops.iter().flat_map(Op::names).any(declared) {
                return false;
            }
            at = edge.to;
        }
        true
    }

    /// This schema with only the keys at `paths`, for a client that reads no others.
    pub fn trimmed(&self, paths: &[&str]) -> Result<Schema, Error> {
        if let Some(path) = paths.iter().find(|path| self.key(path).is_none()) {
            return Err(invalid(format!("key `{path}` is not declared")));
        }
        let mut table: toml::Table =
            toml::from_str(&self.source).map_err(|e| invalid(e.to_string()))?;
        if let Some(Value::Table(keys)) = table.get_mut("keys") {
            for key in self
                .keys
                .iter()
                .filter(|key| !paths.contains(&key.path.as_str()))
            {
                let parent = match key.group.as_str() {
                    "" => Some(&mut *keys),
                    group => keys.get_mut(group).and_then(Value::as_table_mut),
                };
                if let Some(parent) = parent {
                    parent.remove(&key.id);
                }
            }
            keys.retain(|_, value| value.as_table().is_none_or(|table| !table.is_empty()));
        }
        // A group left without keys goes too.
        let kept = |group: &str| {
            paths
                .iter()
                .any(|path| self.key(path).is_some_and(|k| k.group == group))
        };
        if let Some(Value::Table(groups)) = table.get_mut("groups") {
            groups.retain(|group, _| kept(group));
        }
        let text = toml::to_string(&table).map_err(|e| invalid(e.to_string()))?;
        Schema::parse(&text)
    }

    /// The edge that moves a file at `from` toward this version, never past it.
    pub fn edge_from(&self, from: u32) -> Option<&Migration> {
        next_edge(&self.migrations, from, self.version)
    }
}

/// The edge in `edges` that moves a file at `from` toward `to`, never past it.
fn next_edge(edges: &[Migration], from: u32, to: u32) -> Option<&Migration> {
    edges.iter().find(|m| m.from == from && m.to <= to)
}

/// Fail if a retired name is declared again, or a retired asset path leaves the directory.
fn check_retired(
    retired: &Retired,
    keys: &[KeySchema],
    assets: &[AssetSchema],
) -> Result<(), Error> {
    if let Some(key) = keys.iter().find(|key| retired.keys.contains(&key.path)) {
        return Err(invalid(format!("key `{}` is retired", key.path)));
    }
    if let Some(asset) = assets
        .iter()
        .find(|asset| retired.assets.contains(&asset.path))
    {
        let (id, path) = (&asset.id, &asset.path);
        return Err(invalid(format!("asset `{id}` uses retired path `{path}`")));
    }
    if let Some(path) = retired.assets.iter().find(|path| !inside(path)) {
        return Err(invalid(format!("retired asset path `{path}` is invalid")));
    }
    Ok(())
}

/// Read the `keys` table in display order.
fn read_keys(table: BTreeMap<String, Value>, groups: &[Group]) -> Result<Vec<KeySchema>, Error> {
    let mut keys = Vec::new();
    for (segment, value) in table {
        if !groups.iter().any(|group| group.id == segment) {
            keys.push(KeySchema::read(value, &segment, "")?);
            continue;
        }
        let Value::Table(table) = value else {
            return Err(invalid(format!("group `{segment}` must hold keys")));
        };
        for (id, entry) in table {
            keys.push(KeySchema::read(entry, &id, &segment)?);
        }
    }
    // Ungrouped keys have no position, so they sort last.
    let rank = |key: &KeySchema| {
        let position = groups.iter().position(|group| group.id == key.group);
        position.unwrap_or(usize::MAX)
    };
    keys.sort_by(|a, b| (rank(a), a.order, &a.id).cmp(&(rank(b), b.order, &b.id)));
    Ok(keys)
}

/// A schema error with `message`.
fn invalid(message: impl Into<String>) -> Error {
    Kind::Schema(message.into()).into()
}

/// Whether `path` stays inside the namespace directory.
fn inside(path: &str) -> bool {
    !path.is_empty() && !Path::new(path).is_absolute() && !path.split('/').any(|p| p == "..")
}

/// Whether `name` is a single directory name, not `.` or `..`.
fn one_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\0'])
}

/// Check that no env var overrides more than one key or asset.
fn check_env(keys: &[KeySchema], assets: &[AssetSchema]) -> Result<(), Error> {
    let keys = keys
        .iter()
        .filter_map(|key| Some((key.path.as_str(), key.env.as_deref()?)));
    let assets = assets
        .iter()
        .filter_map(|asset| Some((asset.id.as_str(), asset.env.as_deref()?)));
    let mut seen = BTreeSet::new();
    for (owner, var) in keys.chain(assets) {
        if !crate::class::identifier(var, &['_']) {
            return Err(invalid(format!("`{owner}` has an invalid env var `{var}`")));
        }
        if !seen.insert(var) {
            return Err(invalid(format!("env var `{var}` is declared twice")));
        }
    }
    Ok(())
}
