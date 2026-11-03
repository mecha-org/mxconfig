//! Generates a typed Dart client for a schema.

use heck::{ToLowerCamelCase, ToUpperCamelCase};
use mxconfig_schema::{AssetSchema, KeySchema, KeyType, Schema, Value};

use crate::{Scope, default, doc, float, header, ident, key_doc, owner};

/// The Dart package the generated clients use.
const PACKAGE: &str = "mxconfig_dart";

/// Dart keywords, which can't be used as identifiers.
const KEYWORDS: &[&str] = &[
    "assert", "break", "case", "catch", "class", "const", "continue", "default", "do", "else",
    "enum", "extends", "false", "final", "finally", "for", "if", "in", "is", "new", "null",
    "rethrow", "return", "super", "switch", "this", "throw", "true", "try", "var", "void", "while",
    "with",
];

/// Members of every Dart object.
const OBJECT: &[&str] = &["hashCode", "noSuchMethod", "runtimeType", "toString"];

/// The namespace class's own members, and the top-level names its `open` reads.
const NS_MEMBERS: &[&str] = &["changes", "namespace", "open", "schema", "settings"];

/// A group class's own members.
const GROUP_MEMBERS: &[&str] = &["settings"];

/// The members of an enum and of its values.
const ENUM_MEMBERS: &[&str] = &["index", "name", "parse", "value", "values"];

/// What goes before a name that starts with a digit. A leading `_` would make it private.
const LEAD: &str = "$";

/// The namespace's type name.
pub(crate) fn type_name(namespace: &str) -> String {
    crate::type_name(namespace, LEAD)
}

/// `name` as a type name.
fn pascal(name: &str) -> String {
    crate::pascal(name, LEAD)
}

/// The type name for a key's path.
fn path_type(key: &KeySchema) -> String {
    crate::path_type(key, LEAD)
}

/// The type name for an asset.
fn asset_type(asset: &AssetSchema) -> String {
    crate::asset_type(asset, LEAD)
}

/// Emit a typed Dart client for `schema`.
pub fn dart(schema: &Schema) -> Result<String, String> {
    let ns = schema.namespace();
    let ns_ty = type_name(ns);
    let mut types = Scope::new("the library");
    types.claim(ns_ty.clone(), "the namespace")?;

    // Every import has a `$` prefix, which a generated name has only before a digit.
    let mut out = header(schema) + "\nimport 'dart:core' as $core;\n";
    if !schema.assets().is_empty() {
        out += "import 'dart:typed_data' as $typed_data;\n";
    }
    out += &format!("\nimport 'package:{PACKAGE}/mxconfig.dart' as $mx;\n");
    for key in schema.keys() {
        if let KeyType::Enum(values) = &key.ty {
            let ty = types.claim(path_type(key), &owner("key", &key.path))?;
            out += &enumeration(key, &ty, values)?;
        }
    }

    let mut members = Scope::new(format!("`{ns_ty}`"));
    let mut body = String::new();
    let mut groups = String::new();
    for group in schema.groups() {
        let owner = owner("group", &group.id);
        let group_ty = types.claim(pascal(&group.id), &owner)?;
        let name = members.claim(camel(&group.id, NS_MEMBERS), &owner)?;
        let id = doc(&group.id);
        body += &format!("  {group_ty} get {name} => {group_ty}(settings);\n\n");
        let mut scope = Scope::new(format!("`{group_ty}`"));
        let keys: String = schema
            .in_group(&group.id)
            .map(|key| accessors(key, &mut scope, GROUP_MEMBERS))
            .collect::<Result<_, _>>()?;
        let keys = keys.trim_end();
        groups += &format!(
            r"
/// The `{id}` group of `{ns}`.
class {group_ty} {{
  {group_ty}(this.settings);

  final $mx.Settings settings;

{keys}
}}
"
        );
    }
    for key in schema.in_group("") {
        body += &accessors(key, &mut members, NS_MEMBERS)?;
    }
    for asset in schema.assets() {
        body += &asset_accessors(asset, &mut members)?;
    }

    let body = body.trim_end();
    out += &format!(
        r"
/// The namespace this client reads and writes.
const $core.String namespace = {lit};

/// The schema text, which the client reads with.
const $core.String schema ={source};

/// A typed handle on `{ns}`.
class {ns_ty} {{
  {ns_ty}(this.settings);

  final $mx.Settings settings;

  /// Open the namespace with the schema it was generated from.
  static $core.Future<{ns_ty}> open() async =>
      {ns_ty}(await $mx.Settings.open(namespace, schema: schema));

  /// Every change as a typed [{ns_ty}Change].
  $core.Stream<{ns_ty}Change> changes() =>
      settings.watchTyped({ns_ty}Change.from, {ns_ty}Change.fromAsset);

{body}
}}
{groups}{changes}",
        lit = string(ns),
        source = lines(schema.source()),
        changes = changes(schema, &ns_ty, &mut types)?,
    );
    Ok(out)
}

/// `name` as a member name, with `_` added if it is a keyword, an `Object` member or in `taken`.
fn camel(name: &str, taken: &[&str]) -> String {
    ident(
        name.to_lower_camel_case(),
        &[KEYWORDS, OBJECT, taken].concat(),
        LEAD,
    )
}

/// `text` as a single-quoted Dart string literal.
fn string(text: &str) -> String {
    format!("'{}'", escaped(text))
}

/// `text` escaped for use inside a single-quoted Dart string literal.
fn escaped(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '\'' | '\\' | '$' => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.extend(c.escape_unicode()),
            c => out.push(c),
        }
    }
    out
}

/// `text` as adjacent string literals, one per line.
fn lines(text: &str) -> String {
    if text.is_empty() {
        return " ''".into();
    }
    let lines: Vec<String> = text.split_inclusive('\n').map(string).collect();
    format!("\n    {}", lines.join("\n    "))
}

/// The enum type for an enum key.
fn enumeration(key: &KeySchema, ty: &str, values: &[String]) -> Result<String, String> {
    let mut constants = Scope::new(format!("`{ty}`"));
    let mut list = Vec::new();
    for value in values {
        let constant = constants.claim(camel(value, ENUM_MEMBERS), &owner("value", value))?;
        list.push(format!("  {constant}({})", string(value)));
    }
    Ok(format!(
        r"
/// The values `{path}` accepts.
enum {ty} {{
{list};

  const {ty}(this.value);

  /// The value as written in the schema.
  final $core.String value;

  /// The value spelled `text` in the schema, if any.
  static {ty}? parse($core.String? text) =>
      values.where((value) => value.value == text).firstOrNull;
}}
",
        path = doc(&key.path),
        list = list.join(",\n")
    ))
}

/// A key's getter, and its setter and clearer unless it is managed.
fn accessors(key: &KeySchema, scope: &mut Scope, taken: &[&str]) -> Result<String, String> {
    let owner = owner("key", &key.path);
    let base = key.id.to_upper_camel_case();
    let name = scope.claim(camel(&key.id, taken), &owner)?;
    let Kind {
        ty,
        read,
        param,
        write,
        ..
    } = kind(key)?;
    let (doc, lit) = (key_doc(key), string(&key.path));
    let getter = format!("  /// {doc}\n  {ty} get {name} => {read};\n\n");
    // A managed key is only set by vendor and admin files.
    if key.managed {
        return Ok(getter);
    }
    let set = scope.claim(format!("set{base}"), &owner)?;
    let clear = scope.claim(format!("clear{base}"), &owner)?;
    Ok(getter
        + &format!(
            r"  void {set}({param} value) => {write};

  /// Unset it, so a lower layer or the default applies.
  void {clear}() => settings.clear({lit});

"
        ))
}

/// An asset's getter, and its setters unless it is managed.
fn asset_accessors(asset: &AssetSchema, scope: &mut Scope) -> Result<String, String> {
    let owner = owner("asset", &asset.id);
    let base = asset.id.to_upper_camel_case();
    let lit = string(&asset.id);
    let name = scope.claim(format!("asset{base}"), &owner)?;
    let mut out = format!(
        "  /// The bytes of the file that wins, if any layer sets it.\n  $typed_data.Uint8List? {name}() => settings.asset({lit});\n\n"
    );
    if !asset.managed {
        let set = scope.claim(format!("setAsset{base}"), &owner)?;
        let remove = scope.claim(format!("removeAsset{base}"), &owner)?;
        out += &format!(
            r"  void {set}($typed_data.Uint8List bytes) => settings.setAsset({lit}, bytes);

  /// Remove the user's copy, so a lower layer applies again.
  void {remove}() => settings.removeAsset({lit});

"
        );
    }
    Ok(out)
}

/// The sealed change type, with a class per key and asset.
fn changes(schema: &Schema, ns_ty: &str, types: &mut Scope) -> Result<String, String> {
    let base = types.claim(format!("{ns_ty}Change"), "the change type")?;
    let (mut keys, mut assets, mut classes) = (String::new(), String::new(), String::new());
    for key in schema.keys() {
        let class = types.claim(
            format!("{}Change", path_type(key)),
            &owner("key", &key.path),
        )?;
        let Kind { ty, decode, .. } = kind(key)?;
        keys += &format!("        {} => {class}({decode}),\n", string(&key.path));
        classes += &format!(
            r"
/// `{path}` changed.
final class {class} extends {base}<{ty}> {{
  const {class}(super.value);

  @$core.override
  $core.String toString() => '{name} = $value';
}}
",
            path = doc(&key.path),
            name = escaped(&key.path)
        );
    }
    for asset in schema.assets() {
        let owner = owner("asset", &asset.id);
        let class = types.claim(format!("{}Change", asset_type(asset)), &owner)?;
        let (id, name) = (doc(&asset.id), escaped(&asset.id));
        assets += &format!("        '{name}' => const {class}(),\n");
        classes += &format!(
            r"
/// The `{id}` asset's file changed.
final class {class} extends {base}<$core.Null> {{
  const {class}() : super(null);

  @$core.override
  $core.String toString() => '{name} asset changed';
}}
"
        );
    }
    Ok(format!(
        r"
/// A change in `{ns}`, holding the key's new value in its type.
sealed class {base}<T> {{
  const {base}(this.value);

  final T value;

  /// [change] as a typed change, if it is for one of these keys.
  static {base}? from($mx.Change change) => switch (change.key) {{
{keys}        _ => null,
      }};

  /// The typed change for asset [id], if it is one of these assets.
  static {base}? fromAsset($core.String id) => switch (id) {{
{assets}        _ => null,
      }};
}}
{classes}",
        ns = schema.namespace()
    ))
}

/// How a key type is read and written in Dart.
struct Kind {
    /// The getter's type. Nullable unless the key has a default.
    ty: String,
    /// Code that reads the typed value from `settings`.
    read: String,
    /// Code that reads the typed value of a `Change` named `change`.
    decode: String,
    /// The setter's parameter type.
    param: String,
    /// Code that writes `value` through `settings`.
    write: String,
}

/// The generated code for each key type.
fn kind(key: &KeySchema) -> Result<Kind, String> {
    let lit = string(&key.path);
    let enum_ty = path_type(key);
    let is_enum = matches!(key.ty, KeyType::Enum(_));
    // Per type: Dart type, getter method, `Change` field and setter.
    let (ty, getter, field, setter) = match &key.ty {
        KeyType::String => ("$core.String", "getString", "stringValue", "setString"),
        KeyType::Datetime => ("$core.String", "getString", "stringValue", "setToml"),
        KeyType::Integer => ("$core.int", "getInt", "intValue", "setInt"),
        KeyType::Float => ("$core.double", "getDouble", "doubleValue", "setDouble"),
        KeyType::Bool => ("$core.bool", "getBool", "boolValue", "setBool"),
        KeyType::Enum(_) => (enum_ty.as_str(), "getString", "stringValue", "setString"),
        KeyType::Array(_) | KeyType::Map(_) => ("$core.String", "getToml", "value", "setToml"),
    };
    let fallback = fallback(key, &enum_ty)?;
    // Turns code that reads the raw value into code that reads `ty`.
    let typed = |raw: String| {
        let value = if is_enum {
            format!("{enum_ty}.parse({raw})")
        } else {
            raw
        };
        match &fallback {
            Some(or) => format!("{value} ?? {or}"),
            None => value,
        }
    };
    let value = if is_enum { "value.value" } else { "value" };
    Ok(Kind {
        ty: if fallback.is_some() {
            ty.to_string()
        } else {
            format!("{ty}?")
        },
        read: typed(format!("settings.{getter}({lit})")),
        decode: typed(format!("change.{field}")),
        param: ty.to_string(),
        write: format!("settings.{setter}({lit}, {value})"),
    })
}

/// The schema's default as a Dart literal, if there is one.
fn fallback(key: &KeySchema, enum_ty: &str) -> Result<Option<String>, String> {
    Ok(default(key)?.and_then(|default| match default {
        Value::String(s) if matches!(key.ty, KeyType::Enum(_)) => {
            Some(format!("{enum_ty}.{}", camel(s, ENUM_MEMBERS)))
        }
        Value::String(s) => Some(string(s)),
        Value::Datetime(d) => Some(string(&d.to_string())),
        Value::Integer(i) => Some(i.to_string()),
        Value::Float(f) => Some(float(*f, FLOATS)),
        Value::Boolean(b) => Some(b.to_string()),
        Value::Array(_) | Value::Table(_) => None,
    }))
}

/// Dart's names for NaN, infinity and negative infinity.
const FLOATS: [&str; 3] = [
    "$core.double.nan",
    "$core.double.infinity",
    "$core.double.negativeInfinity",
];

#[cfg(test)]
mod tests {
    use super::PACKAGE;

    #[test]
    fn the_package_name_matches_the_pubspec() {
        let pubspec = include_str!("../../../packages/mxconfig_dart/pubspec.yaml");
        assert!(pubspec.lines().any(|l| l == format!("name: {PACKAGE}")));
    }
}
