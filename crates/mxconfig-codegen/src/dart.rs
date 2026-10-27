//! Generates a typed Dart client for a schema.

use heck::{ToLowerCamelCase, ToUpperCamelCase};
use mxconfig_schema::{AssetSchema, KeySchema, KeyType, Schema, Value};

use crate::{
    Scope, asset_type, default, doc, float, header, ident, key_doc, owner, pascal, path_type,
    type_name,
};

/// The Dart package the generated clients use.
const PACKAGE: &str = "mxconfig_dart";

/// Dart keywords, which can't be used as identifiers.
const KEYWORDS: &[&str] = &[
    "assert", "break", "case", "catch", "class", "const", "continue", "default", "do", "else",
    "enum", "extends", "false", "final", "finally", "for", "if", "in", "is", "new", "null",
    "rethrow", "return", "super", "switch", "this", "throw", "true", "try", "var", "void", "while",
    "with",
];

/// Top-level names already used by the package and Dart core.
const IMPORTED: &[&str] = &[
    "Change",
    "DaemonPresence",
    "DetachCause",
    "Describe",
    "Diagnostic",
    "GroupInfo",
    "KeyInfo",
    "MxconfigException",
    "Settings",
    "String",
    "Uint8List",
    "bool",
    "double",
    "int",
    "namespace",
    "schema",
];

/// Members of every Dart object.
const OBJECT: &[&str] = &["hashCode", "noSuchMethod", "runtimeType", "toString"];

/// Emit a typed Dart client for `schema`.
pub fn dart(schema: &Schema) -> Result<String, String> {
    let ns = schema.namespace();
    let ns_ty = type_name(ns);
    let mut types = Scope::new("the library", IMPORTED);
    types.claim(ns_ty.clone(), "the namespace")?;

    let typed_data = if schema.assets().is_empty() {
        ""
    } else {
        "import 'dart:typed_data';\n"
    };
    let mut out = header(schema) + "\n" + typed_data;
    out += &format!("import 'package:{PACKAGE}/mxconfig.dart';\n");
    for key in schema.keys() {
        if let KeyType::Enum(values) = &key.ty {
            let ty = types.claim(path_type(key), &owner("key", &key.path))?;
            out += &enumeration(key, &ty, values)?;
        }
    }

    let reserved = [
        OBJECT,
        &["changes", "namespace", "open", "schema", "settings"],
    ]
    .concat();
    let mut members = Scope::new(format!("`{ns_ty}`"), &reserved);
    let mut body = String::new();
    let mut groups = String::new();
    for group in schema.groups() {
        let owner = owner("group", &group.id);
        let group_ty = types.claim(pascal(&group.id), &owner)?;
        let name = members.claim(camel(&group.id), &owner)?;
        let id = doc(&group.id);
        body += &format!("  {group_ty} get {name} => {group_ty}(settings);\n\n");
        let mut scope = Scope::new(format!("`{group_ty}`"), &[OBJECT, &["settings"]].concat());
        let keys: String = schema
            .in_group(&group.id)
            .map(|key| accessors(key, &mut scope))
            .collect::<Result<_, _>>()?;
        let keys = keys.trim_end();
        groups += &format!(
            r"
/// The `{id}` group of `{ns}`.
class {group_ty} {{
  {group_ty}(this.settings);

  final Settings settings;

{keys}
}}
"
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
        r"
/// The namespace this client reads and writes.
const String namespace = {lit};

/// The schema text, which the client reads with.
const String schema ={source};

/// A typed handle on `{ns}`.
class {ns_ty} {{
  {ns_ty}(this.settings);

  final Settings settings;

  /// Open the namespace with the schema it was generated from.
  static Future<{ns_ty}> open() async =>
      {ns_ty}(await Settings.open(namespace, schema: schema));

  /// Every change as a typed [{ns_ty}Change].
  Stream<{ns_ty}Change> changes() =>
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

/// `name` as a member name.
fn camel(name: &str) -> String {
    ident(name.to_lower_camel_case(), KEYWORDS)
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
    let reserved = [OBJECT, &["index", "name", "parse", "value", "values"]].concat();
    let mut constants = Scope::new(format!("`{ty}`"), &reserved);
    let mut list = Vec::new();
    for value in values {
        let constant = constants.claim(camel(value), &owner("value", value))?;
        list.push(format!("  {constant}({})", string(value)));
    }
    Ok(format!(
        r"
/// The values `{path}` accepts.
enum {ty} {{
{list};

  const {ty}(this.value);

  /// The value as written in the schema.
  final String value;

  /// The value spelled `text` in the schema, if any.
  static {ty}? parse(String? text) =>
      values.where((value) => value.value == text).firstOrNull;
}}
",
        path = doc(&key.path),
        list = list.join(",\n")
    ))
}

/// A key's getter, and its setter and clearer unless it is managed.
fn accessors(key: &KeySchema, scope: &mut Scope) -> Result<String, String> {
    let owner = owner("key", &key.path);
    let base = key.id.to_upper_camel_case();
    let name = scope.claim(camel(&key.id), &owner)?;
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
            r"  Future<void> {set}({param} value) => {write};

  /// Unset it, so a lower layer or the default applies.
  Future<void> {clear}() => settings.remove({lit});

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
        "  /// The bytes of the file that wins, if any layer sets it.\n  Uint8List? {name}() => settings.asset({lit});\n\n"
    );
    if !asset.managed {
        let set = scope.claim(format!("setAsset{base}"), &owner)?;
        let clear = scope.claim(format!("clearAsset{base}"), &owner)?;
        out += &format!(
            r"  Future<void> {set}(Uint8List bytes) => settings.setAsset({lit}, bytes);

  /// Remove the user's copy, so a lower layer applies again.
  Future<void> {clear}() => settings.removeAsset({lit});

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

  @override
  String toString() => '{name} = $value';
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
final class {class} extends {base}<Null> {{
  const {class}() : super(null);

  @override
  String toString() => '{name} asset changed';
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
  static {base}? from(Change change) => switch (change.key) {{
{keys}        _ => null,
      }};

  /// The typed change for asset [id], if it is one of these assets.
  static {base}? fromAsset(String id) => switch (id) {{
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
        KeyType::String => ("String", "getString", "stringValue", "setString"),
        KeyType::Datetime => ("String", "getString", "stringValue", "setToml"),
        KeyType::Integer => ("int", "getInt", "intValue", "setInt"),
        KeyType::Float => ("double", "getDouble", "doubleValue", "setDouble"),
        KeyType::Bool => ("bool", "getBool", "boolValue", "setBool"),
        KeyType::Enum(_) => (enum_ty.as_str(), "getString", "stringValue", "setString"),
        KeyType::Array(_) | KeyType::Map(_) => ("String", "getToml", "value", "setToml"),
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
            Some(format!("{enum_ty}.{}", camel(s)))
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
const FLOATS: [&str; 3] = ["double.nan", "double.infinity", "double.negativeInfinity"];

#[cfg(test)]
mod tests {
    use super::{IMPORTED, PACKAGE};

    #[test]
    fn the_package_name_matches_the_pubspec() {
        let pubspec = include_str!("../../../packages/mxconfig_dart/pubspec.yaml");
        assert!(pubspec.lines().any(|l| l == format!("name: {PACKAGE}")));
    }

    /// The package's library file.
    const LIBRARY: &str = include_str!("../../../packages/mxconfig_dart/lib/mxconfig.dart");
    /// The library's parts, by path under `lib`.
    const PARTS: [(&str, &str); 1] = [(
        "src/source.dart",
        include_str!("../../../packages/mxconfig_dart/lib/src/source.dart"),
    )];

    /// The statements in `code` that start with `keyword`, each up to its end.
    fn statements<'a>(code: &'a str, keyword: &'a str) -> impl Iterator<Item = &'a str> {
        code.split(';')
            .filter_map(move |s| s.trim().strip_prefix(keyword))
    }

    /// The public top-level types `code` declares.
    fn declared(code: &str) -> Vec<&str> {
        let kinds = ["class ", "enum ", "typedef ", "mixin "];
        code.lines()
            // Top-level declarations are not indented.
            .filter(|line| !line.starts_with(char::is_whitespace) && !line.starts_with("//"))
            .filter_map(|line| {
                let found = kinds
                    .iter()
                    .filter_map(|kind| line.find(kind).map(|at| at + kind.len()));
                let rest = &line[found.min()?..];
                rest.split(|c: char| !c.is_alphanumeric() && c != '_')
                    .next()
            })
            .filter(|name| !name.is_empty() && !name.starts_with('_'))
            .collect()
    }

    #[test]
    fn imported_names_every_public_type_of_the_package() {
        let parts: Vec<&str> = statements(LIBRARY, "part ").collect();
        let known: Vec<String> = PARTS.iter().map(|(path, _)| format!("'{path}'")).collect();
        assert_eq!(parts, known, "PARTS must list the library's parts");

        let shown = statements(LIBRARY, "export ")
            .filter_map(|export| export.split_once(" show "))
            .flat_map(|(_, names)| names.split(',').map(str::trim));
        let codes = std::iter::once(LIBRARY).chain(PARTS.iter().map(|(_, code)| *code));
        let names: Vec<&str> = shown.chain(codes.flat_map(declared)).collect();
        assert!(names.contains(&"Settings"), "{names:?}");
        for name in names {
            assert!(IMPORTED.contains(&name), "IMPORTED lacks `{name}`");
        }
    }
}
