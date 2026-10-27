//! Generates a typed Rust client for a schema.

use heck::ToSnakeCase;
use mxconfig_schema::{AssetSchema, KeySchema, KeyType, Schema, Value};

use crate::{
    Scope, asset_type, default, doc, float, header, ident, key_doc, owner, pascal, path_type,
    type_name,
};

/// Rust keywords, which can't be used as identifiers.
const KEYWORDS: &[&str] = &[
    "Self", "abstract", "as", "async", "await", "become", "box", "break", "const", "continue",
    "crate", "do", "dyn", "else", "enum", "extern", "false", "final", "fn", "for", "gen", "if",
    "impl", "in", "let", "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub",
    "ref", "return", "self", "static", "struct", "super", "trait", "true", "try", "type", "typeof",
    "unsafe", "unsized", "use", "virtual", "where", "while", "yield",
];

/// The return type of a write.
const TICKET: &str = "Result<mxconfig::Ticket<mxconfig::Write>, mxconfig::Error>";

/// Emit a typed Rust client for `schema`.
pub fn rust(schema: &Schema) -> Result<String, String> {
    let ns = schema.namespace();
    let ns_ty = type_name(ns);
    let mut types = Scope::new("the module", &[]);
    types.claim(ns_ty.clone(), "the namespace")?;
    types.claim(format!("{ns_ty}Change"), "the change enum")?;

    let mut out = header(schema);
    for key in schema.keys() {
        if let KeyType::Enum(values) = &key.ty {
            let ty = types.claim(path_type(key), &owner("key", &key.path))?;
            out += &enumeration(key, &ty, values)?;
        }
    }

    let reserved = ["from_settings", "settings"];
    let mut members = Scope::new(format!("`{ns_ty}`"), &reserved);
    let mut body = String::new();
    let mut groups = String::new();
    for group in schema.groups() {
        let owner = owner("group", &group.id);
        let group_ty = types.claim(pascal(&group.id), &owner)?;
        let name = members.claim(snake(&group.id), &owner)?;
        body += &format!(
            r"    pub fn {name}(&self) -> {group_ty} {{
        {group_ty} {{
            settings: self.settings.clone(),
        }}
    }}

"
        );
        let mut scope = Scope::new(format!("`{group_ty}`"), &["settings"]);
        let keys: String = schema
            .in_group(&group.id)
            .map(|key| accessors(key, &mut scope))
            .collect::<Result<_, _>>()?;
        groups += &format!(
            r#"
/// The `{id}` group of `{ns}`.
#[derive(Debug, Clone)]
#[allow(dead_code, reason = "an app may use only part of a client")]
pub struct {group_ty} {{
    settings: mxconfig::Settings,
}}

#[allow(dead_code, reason = "an app may use only part of a client")]
impl {group_ty} {{
{keys}
}}
"#,
            id = doc(&group.id),
            keys = keys.trim_end()
        );
    }
    for key in schema.in_group("") {
        body += &accessors(key, &mut members)?;
    }
    for asset in schema.assets() {
        body += &asset_accessors(asset, &mut members)?;
    }

    let body = body.trim_end();
    out += &format!(
        r#"
/// A typed handle on `{ns}`.
#[derive(Debug, Clone)]
pub struct {ns_ty} {{
    settings: mxconfig::Settings,
}}

#[allow(dead_code, reason = "an app may use only part of a client")]
impl {ns_ty} {{
    /// The untyped handle: its schema, revision and diagnostics.
    pub fn settings(&self) -> &mxconfig::Settings {{
        &self.settings
    }}

{body}
}}
"#
    );
    out += &changes(schema, &ns_ty)?;
    out += &groups;
    Ok(out)
}

/// `name` as a function or field name.
fn snake(name: &str) -> String {
    ident(name.to_snake_case(), KEYWORDS)
}

/// The enum type for an enum key.
fn enumeration(key: &KeySchema, ty: &str, values: &[String]) -> Result<String, String> {
    let mut variants = Scope::new(format!("`{ty}`"), &[]);
    let default = key.default.as_ref().and_then(Value::as_str);
    let (mut decls, mut spell, mut parse) = (String::new(), String::new(), String::new());
    for value in values {
        let variant = variants.claim(pascal(value), &owner("value", value))?;
        if default == Some(value.as_str()) {
            decls += "    #[default]\n";
        }
        decls += &format!("    {variant},\n");
        spell += &format!("            {ty}::{variant} => {value:?},\n");
        parse += &format!("            {value:?} => Some({ty}::{variant}),\n");
    }
    let derive = match default {
        Some(_) => "Debug, Clone, Copy, PartialEq, Eq, Default",
        None => "Debug, Clone, Copy, PartialEq, Eq",
    };
    Ok(format!(
        r#"
/// The values `{path}` accepts.
#[derive({derive})]
#[allow(dead_code, reason = "an app may use only part of a client")]
pub enum {ty} {{
{decls}}}

#[allow(dead_code, reason = "an app may use only part of a client")]
impl {ty} {{
    /// The value as written in the schema.
    pub fn as_str(self) -> &'static str {{
        match self {{
{spell}        }}
    }}

    /// The value spelled `text` in the schema, if any.
    pub fn parse(text: &str) -> Option<Self> {{
        match text {{
{parse}            _ => None,
        }}
    }}
}}
"#,
        path = doc(&key.path),
    ))
}

/// A key's getter, and its setter and clearer unless it is managed.
fn accessors(key: &KeySchema, scope: &mut Scope) -> Result<String, String> {
    let owner = owner("key", &key.path);
    let base = key.id.to_snake_case();
    let name = scope.claim(snake(&key.id), &owner)?;
    let Kind {
        ty,
        param,
        read,
        write,
    } = kind(key)?;
    let (doc, lit) = (key_doc(key), format!("{:?}", key.path));
    let getter = format!(
        r"    /// {doc}
    pub fn {name}(&self) -> {ty} {{
        self.settings.get({lit}){read}
    }}

"
    );
    // A managed key is only set by vendor and admin files.
    if key.managed {
        return Ok(getter);
    }
    let set = scope.claim(format!("set_{base}"), &owner)?;
    let clear = scope.claim(format!("clear_{base}"), &owner)?;
    Ok(getter
        + &format!(
            r"    pub fn {set}(&self, value: {param}) -> {TICKET} {{
        self.settings.write(&[mxconfig::Edit::set({lit}, {write})])
    }}

    /// Unset it, so a lower layer or the default applies.
    pub fn {clear}(&self) -> {TICKET} {{
        self.settings.write(&[mxconfig::Edit::remove({lit})])
    }}

"
        ))
}

/// An asset's getter, and its setters unless it is managed.
fn asset_accessors(asset: &AssetSchema, scope: &mut Scope) -> Result<String, String> {
    let owner = owner("asset", &asset.id);
    let base = asset.id.to_snake_case();
    let lit = format!("{:?}", asset.id);
    let name = scope.claim(format!("asset_{base}"), &owner)?;
    let mut out = format!(
        r"    /// The file that wins for this asset, if any layer sets it.
    pub fn {name}(&self) -> Option<mxconfig::Asset> {{
        self.settings.asset({lit})
    }}

"
    );
    if !asset.managed {
        let set = scope.claim(format!("set_asset_{base}"), &owner)?;
        let from = scope.claim(format!("set_asset_{base}_from"), &owner)?;
        let clear = scope.claim(format!("clear_asset_{base}"), &owner)?;
        out += &format!(
            r"    pub fn {set}(&self, bytes: &[u8]) -> {TICKET} {{
        self.settings.write_asset({lit}, bytes)
    }}

    /// Copy a regular file as the user's copy.
    pub fn {from}(&self, file: std::os::fd::OwnedFd) -> {TICKET} {{
        self.settings.write_asset_from({lit}, file)
    }}

    /// Remove the user's copy, so a lower layer applies again.
    pub fn {clear}(&self) -> {TICKET} {{
        self.settings.clear_asset({lit})
    }}

"
        );
    }
    Ok(out)
}

/// The change enum and the `Namespace` impl that produces it.
fn changes(schema: &Schema, ns_ty: &str) -> Result<String, String> {
    let mut variants = Scope::new(format!("`{ns_ty}Change`"), &[]);
    let (mut list, mut keys, mut assets) = (String::new(), String::new(), String::new());
    for key in schema.keys() {
        let variant = variants.claim(path_type(key), &owner("key", &key.path))?;
        let Kind { ty, read, .. } = kind(key)?;
        list += &format!("    {variant}({ty}),\n");
        keys += &format!(
            "            {:?} => {ns_ty}Change::{variant}(change.value.clone(){read}),\n",
            key.path
        );
    }
    for asset in schema.assets() {
        let variant = variants.claim(asset_type(asset), &owner("asset", &asset.id))?;
        list += &format!("    {variant},\n");
        assets += &format!(
            "            {:?} => Some({ns_ty}Change::{variant}),\n",
            asset.id
        );
    }
    let change = if keys.is_empty() {
        format!("fn change(_: &mxconfig::Change) -> Option<{ns_ty}Change> {{\n        None\n    }}")
    } else {
        format!(
            "fn change(change: &mxconfig::Change) -> Option<{ns_ty}Change> {{
        Some(match change.key.as_str() {{
{keys}            _ => return None,
        }})
    }}"
        )
    };
    // Without assets, the trait's default says there are none.
    let asset_change = if assets.is_empty() {
        String::new()
    } else {
        format!(
            "

    fn asset_change(id: &str) -> Option<{ns_ty}Change> {{
        match id {{
{assets}            _ => None,
        }}
    }}"
        )
    };
    Ok(format!(
        r"
/// A change in `{ns}`, decoded to the key's type.
#[derive(Debug, Clone, PartialEq)]
pub enum {ns_ty}Change {{
{list}}}

impl mxconfig::FromSettings for {ns_ty} {{
    fn from_settings(settings: mxconfig::Settings) -> Self {{
        Self {{ settings }}
    }}
}}

impl mxconfig::Namespace for {ns_ty} {{
    const NAMESPACE: &'static str = {ns:?};
    const SCHEMA: &'static str = {source};
    type Change = {ns_ty}Change;

    {change}{asset_change}
}}
",
        ns = schema.namespace(),
        source = raw(schema.source())
    ))
}

/// How a key type is read and written in Rust.
struct Kind {
    /// The getter's type. Optional unless the key has a default.
    ty: String,
    /// The setter's parameter type.
    param: String,
    /// Code that converts an `Option<Value>` to `ty`.
    read: String,
    /// Code that builds the `mxconfig::Value` a setter writes.
    write: String,
}

/// The generated code for each key type.
fn kind(key: &KeySchema) -> Result<Kind, String> {
    let enum_ty = path_type(key);
    let enum_read = format!("v.as_str().and_then({enum_ty}::parse)");
    // Per type: the Rust type, its `mxconfig::Value` variant, and code reading `v`.
    let (ty, variant, read) = match &key.ty {
        KeyType::String => ("String", "String", "v.as_str().map(str::to_string)"),
        KeyType::Datetime => ("mxconfig::Datetime", "Datetime", "v.as_datetime().copied()"),
        KeyType::Integer => ("i64", "Integer", "v.as_integer()"),
        KeyType::Float => ("f64", "Float", "mxconfig::value::as_f64(&v)"),
        KeyType::Bool => ("bool", "Boolean", "v.as_bool()"),
        KeyType::Enum(_) => (enum_ty.as_str(), "String", enum_read.as_str()),
        KeyType::Array(_) => ("Vec<mxconfig::Value>", "Array", "v.as_array().cloned()"),
        KeyType::Map(_) => ("mxconfig::Table", "Table", "v.as_table().cloned()"),
    };
    // The setter's parameter, and the variant's payload built from it.
    let (param, payload) = match key.ty {
        KeyType::String => ("impl Into<String>", "value.into()"),
        KeyType::Enum(_) => (ty, "value.as_str().to_string()"),
        _ => (ty, "value"),
    };
    let read = format!(".and_then(|v| {read})");
    let (ty, read) = match fallback(key, &enum_ty)? {
        Some(fallback) => (ty.to_string(), format!("{read}{fallback}")),
        None => (format!("Option<{ty}>"), read),
    };
    Ok(Kind {
        ty,
        param: param.to_string(),
        read,
        write: format!("mxconfig::Value::{variant}({payload})"),
    })
}

/// Code for the schema's default, used when a getter finds no value.
fn fallback(key: &KeySchema, enum_ty: &str) -> Result<Option<String>, String> {
    Ok(default(key)?.map(|default| match default {
        Value::String(s) if matches!(key.ty, KeyType::Enum(_)) => {
            format!(".unwrap_or({enum_ty}::{})", pascal(s))
        }
        Value::String(s) if s.is_empty() => ".unwrap_or_default()".to_string(),
        Value::String(s) => format!(".unwrap_or_else(|| {s:?}.to_string())"),
        Value::Integer(i) => format!(".unwrap_or({i})"),
        Value::Float(f) => format!(".unwrap_or({})", float(*f, FLOATS)),
        Value::Boolean(b) => format!(".unwrap_or({b})"),
        Value::Datetime(d) => format!(
            ".unwrap_or_else(|| {:?}.parse().expect(\"the schema's default\"))",
            d.to_string()
        ),
        Value::Array(_) | Value::Table(_) => ".unwrap_or_default()".to_string(),
    }))
}

/// Rust's names for NaN, infinity and negative infinity.
const FLOATS: [&str; 3] = ["f64::NAN", "f64::INFINITY", "f64::NEG_INFINITY"];

/// `text` as a raw string literal, with enough `#`s to hold it.
fn raw(text: &str) -> String {
    let mut hashes = String::from("#");
    while text.contains(&format!("\"{hashes}")) {
        hashes.push('#');
    }
    format!("r{hashes}\"{text}\"{hashes}")
}
