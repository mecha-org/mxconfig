//! Tests that the generated clients have the documented API.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use mxconfig_schema::Schema;

const SCHEMA: &str = r##"
namespace = "org.mechanix.demo"
version = 2

[groups.colors]
label = "Colors"
order = 10

[keys.colors.accent]
type = "string"
default = "#7aa2f7"
label = "Accent"
description = "The accent color."

[keys.colors.corner_radius]
type = "integer"
default = 8
label = "Corner radius"
description = "Corner radius in pixels."
min = 0
max = 64

[keys.colors.opacity]
type = "float"
default = 0.8
label = "Opacity"
description = "Window opacity."

[keys.colors.mode]
type = "enum"
values = ["light", "dark"]
default = "dark"
label = "Mode"
description = "Theme mode."

[keys.verbose]
type = "bool"
default = false
label = "Verbose"
description = "Verbose logging."

[keys.nickname]
type = "string"
label = "Nickname"
description = "Display name; unset until the user picks one."

[keys.recent]
type = "array"
element = "string"
label = "Recent"
description = "Recently opened items."
"##;

fn schema() -> Schema {
    Schema::parse(SCHEMA).expect("schema parses")
}

/// The Dart package's name and source.
fn runtime() -> (String, String) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/mxconfig_dart");
    let pubspec = fs::read_to_string(dir.join("pubspec.yaml")).expect("the Dart package exists");
    let name = pubspec
        .lines()
        .find_map(|line| line.strip_prefix("name:"))
        .expect("the pubspec names the package")
        .trim()
        .to_string();
    let library = fs::read_to_string(dir.join("lib/mxconfig.dart")).expect("the runtime source");
    (name, library)
}

/// The members the generated Dart uses on `settings`, as calls and fields.
fn reachable(code: &str) -> (BTreeSet<String>, BTreeSet<String>) {
    let (mut calls, mut fields) = (BTreeSet::new(), BTreeSet::new());
    let mut rest = code;
    while let Some(at) = rest.find("settings.") {
        rest = &rest[at + "settings.".len()..];
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if name.is_empty() {
            continue;
        }
        if rest[name.len()..].starts_with('(') {
            calls.insert(name);
        } else {
            fields.insert(name);
        }
    }
    (calls, fields)
}

/// Assert that `code` contains each of `parts`.
#[track_caller]
fn has(code: &str, parts: &[&str]) {
    for part in parts {
        assert!(code.contains(part), "missing {part:?} in:\n{code}");
    }
}

/// Assert that `code` contains none of `parts`.
#[track_caller]
fn lacks(code: &str, parts: &[&str]) {
    for part in parts {
        assert!(!code.contains(part), "unexpected {part:?} in:\n{code}");
    }
}

#[test]
fn the_rust_face_is_typed_and_complete() {
    let code = mxconfig_codegen::rust(&schema()).unwrap();

    // The namespace and schema are on the trait, and opening is on the `Source`.
    lacks(
        &code,
        &["pub const NAMESPACE", "pub fn open(", "pub fn watch("],
    );
    has(
        &code,
        &[
            "const NAMESPACE: &'static str = \"org.mechanix.demo\";",
            "const SCHEMA: &'static str = r#",
            "pub struct Demo {",
            "impl mxconfig::FromSettings for Demo {",
            "impl mxconfig::Namespace for Demo {",
            // Changes carry the key's type.
            "pub enum DemoChange {",
            "    ColorsAccent(String),",
            "    ColorsMode(ColorsMode),",
            "    Nickname(Option<String>),",
            "\"colors.accent\" => DemoChange::ColorsAccent(",
            // Writes return a ticket.
            "-> Result<mxconfig::Ticket<mxconfig::Write>, mxconfig::Error> {",
            // Groups have accessors with typed getters and setters.
            "pub fn colors(&self) -> Colors {",
            "pub fn accent(&self) -> String {",
            "pub fn set_corner_radius(&self, value: i64)",
            "pub fn clear_corner_radius(&self)",
            "pub fn opacity(&self) -> f64 {",
            // Enums are types with the schema's value names.
            "pub enum ColorsMode {",
            "ColorsMode::Light => \"light\",",
            "#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]",
            "    #[default]\n    Dark,",
            "mxconfig::Value::String(value.as_str().to_string())",
            // A key without a default is optional. Arrays are `Vec<Value>`.
            "pub fn nickname(&self) -> Option<String> {",
            "pub fn recent(&self) -> Option<Vec<mxconfig::Value>> {",
            "pub fn verbose(&self) -> bool {",
            // Enums parse and print their schema spelling.
            "pub fn as_str(self) -> &'static str {",
            "pub fn parse(text: &str) -> Option<Self> {",
            "\"dark\" => Some(ColorsMode::Dark),",
        ],
    );
    // Schema and revision are on the untyped handle, reached from the client only.
    assert_eq!(
        code.matches("pub fn settings(&self) -> &mxconfig::Settings")
            .count(),
        1,
        "{code}"
    );
    lacks(
        &code,
        &[
            "pub fn describe(",
            "pub fn revision(",
            "impl std::fmt::Display for ColorsMode",
            "impl std::str::FromStr for ColorsMode",
            // No assets, so the trait's default asset change is kept.
            "fn asset_change(",
        ],
    );
}

#[test]
fn the_dart_face_is_typed_and_complete() {
    let code = mxconfig_codegen::dart(&schema()).unwrap();
    let (name, _) = runtime();
    has(&code, &[&format!("import 'package:{name}/mxconfig.dart';")]);
    has(
        &code,
        &[
            "const String namespace = 'org.mechanix.demo';",
            "class Demo {",
            "static Future<Demo> open() async =>",
            "Demo(await Settings.open(namespace, schema: schema));",
            "    'namespace = \"org.mechanix.demo\"\\n'\n",
            "class Colors {",
            "Colors get colors => Colors(settings);",
            "String get accent => settings.getString('colors.accent') ?? '#7aa2f7';",
            "int get cornerRadius => settings.getInt('colors.corner_radius') ?? 8;",
            // Setters are typed and call the matching runtime method.
            "Future<void> setAccent(String value) => settings.setString('colors.accent', value);",
            "Future<void> clearAccent() => settings.remove('colors.accent');",
            "enum ColorsMode {",
            "light('light'),",
            "dark('dark');",
            "static ColorsMode? parse(String? text)",
            "ColorsMode get mode => ColorsMode.parse(settings.getString('colors.mode')) ?? ColorsMode.dark;",
            "Future<void> setMode(ColorsMode value) => settings.setString('colors.mode', value.value);",
            // Changes are a sealed type with a typed class per key.
            "sealed class DemoChange<T> {",
            "  const DemoChange(this.value);\n\n  final T value;",
            "'colors.mode' => ColorsModeChange(ColorsMode.parse(change.stringValue) ?? ColorsMode.dark),",
            "final class NicknameChange extends DemoChange<String?> {\n  const NicknameChange(super.value);\n\n  @override\n  String toString() => 'nickname = $value';\n}",
            "final class ColorsModeChange extends DemoChange<ColorsMode> {",
            "settings.watchTyped(DemoChange.from, DemoChange.fromAsset);",
            // No assets, so no asset change.
            "static DemoChange? fromAsset(String id) => switch (id) {\n        _ => null,\n      };",
            // No default means a nullable getter. Arrays are read as TOML text.
            "String? get nickname => settings.getString('nickname');",
            "String? get recent => settings.getToml('recent');",
        ],
    );
    // Schema and changes are reached through `settings` and `changes()`.
    lacks(&code, &["watch()", "describe("]);
}

/// Generated Dart only uses members the package defines.
#[test]
fn the_dart_face_only_reaches_for_the_runtime_it_imports() {
    let code = mxconfig_codegen::dart(&schema()).unwrap();
    let (_, library) = runtime();
    let (calls, fields) = reachable(&code);

    assert!(!calls.is_empty(), "{code}");
    for call in calls {
        assert!(
            [" {call}(", " {call}<"]
                .iter()
                .any(|form| library.contains(&form.replace("{call}", &call))),
            "generated code calls settings.{call}(), which `{library:?}` does not define"
        );
    }
    for field in fields {
        assert!(
            library.contains(&format!(" {field} =>")),
            "generated code reads settings.{field}, which the runtime does not expose"
        );
    }
}

/// The committed Dart examples match the generator's output.
#[test]
fn the_committed_dart_example_matches_the_emitter() {
    let schemas = [
        include_str!("../../mxconfig-example/schema/org.mechanix.demo.toml"),
        include_str!("../../mxconfig-example/schema/org.mechanix.desktop.display.toml"),
    ];
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/mxconfig_dart/example/generated");

    for text in schemas {
        let schema = Schema::parse(text).expect("the example schema parses");
        let (name, body) = mxconfig_codegen::dart_file(&schema).unwrap();
        let committed = fs::read_to_string(path.join(&name))
            .unwrap_or_else(|e| panic!("{} is committed: {e}", path.join(&name).display()));
        assert_eq!(committed, body, "{name} is stale; regenerate it");
    }
}

#[test]
fn file_names_follow_the_namespace() {
    let files = mxconfig_codegen::all(&schema()).unwrap();
    let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, vec!["Demo.rs", "demo.dart"]);
}

#[test]
fn an_asset_gets_accessors_in_both_faces() {
    const WITH_ASSET: &str = r#"
namespace = "org.mechanix.demo"
version = 1

[assets.layout]
path = "ui/layout.yaml"

[keys.brightness]
type = "integer"
default = 50
label = "Brightness"
description = "Panel brightness."
"#;
    let schema = Schema::parse(WITH_ASSET).unwrap();

    let rust = mxconfig_codegen::rust(&schema).unwrap();
    has(
        &rust,
        &[
            "pub fn asset_layout(&self) -> Option<mxconfig::Asset>",
            "pub fn set_asset_layout",
            "pub fn set_asset_layout_from(&self, file: std::os::fd::OwnedFd)",
            "pub fn clear_asset_layout",
            // Asset changes are typed too.
            "    AssetLayout,",
            "fn asset_change(id: &str) -> Option<DemoChange> {",
            "\"layout\" => Some(DemoChange::AssetLayout),",
        ],
    );

    let dart = mxconfig_codegen::dart(&schema).unwrap();
    has(
        &dart,
        &[
            "Uint8List? assetLayout()",
            "Future<void> setAssetLayout",
            "import 'dart:typed_data';",
            "'layout' => const AssetLayoutChange(),",
            "final class AssetLayoutChange extends DemoChange<Null> {\n  const AssetLayoutChange() : super(null);",
        ],
    );
}

/// One-key schemas for the naming tests.
fn keyed(keys: &str) -> Schema {
    let src = format!("namespace = \"org.mechanix.demo\"\nversion = 1\n\n{keys}");
    Schema::parse(&src).unwrap()
}

#[test]
fn strings_from_the_schema_are_escaped_in_both_faces() {
    let schema = keyed(
        r#"
[keys."it's"]
type = "enum"
values = ["it's", "say \"hi\"", "$HOME"]
default = "it's"
label = "Quote"
description = "d"
"#,
    );
    let rust = mxconfig_codegen::rust(&schema).unwrap();
    has(&rust, &[r#""say \"hi\"" => Some("#]);
    let dart = mxconfig_codegen::dart(&schema).unwrap();
    has(
        &dart,
        &[r"('it\'s')", r"('\$HOME')", r"settings.getString('it\'s')"],
    );
}

#[test]
fn a_keyword_key_gets_a_usable_name() {
    let schema = keyed(
        "[keys.match]\ntype = \"bool\"\nlabel = \"M\"\ndescription = \"d\"\n\n\
         [keys.default]\ntype = \"bool\"\nlabel = \"D\"\ndescription = \"d\"\n",
    );
    let rust = mxconfig_codegen::rust(&schema).unwrap();
    has(&rust, &["pub fn match_(&self)", "pub fn set_match(&self"]);
    let dart = mxconfig_codegen::dart(&schema).unwrap();
    has(&dart, &["bool? get default_ =>"]);
}

#[test]
fn names_that_would_collide_are_refused() {
    let twins = keyed(
        "[keys.corner_radius]\ntype = \"bool\"\nlabel = \"A\"\ndescription = \"d\"\n\n\
         [keys.corner-radius]\ntype = \"bool\"\nlabel = \"B\"\ndescription = \"d\"\n",
    );
    let err = mxconfig_codegen::rust(&twins).unwrap_err();
    assert!(err.contains("corner_radius"), "{err}");
    assert!(mxconfig_codegen::dart(&twins).is_err());

    // A key can't use a name the generated API already uses.
    let reserved = keyed("[keys.settings]\ntype = \"bool\"\nlabel = \"S\"\ndescription = \"d\"\n");
    assert!(mxconfig_codegen::rust(&reserved).is_err());
    assert!(mxconfig_codegen::dart(&reserved).is_err());
}

#[test]
fn a_datetime_key_is_typed_in_rust_and_text_in_dart() {
    let schema = keyed("[keys.seen]\ntype = \"datetime\"\nlabel = \"S\"\ndescription = \"d\"\n");
    let rust = mxconfig_codegen::rust(&schema).unwrap();
    has(
        &rust,
        &[
            "pub fn seen(&self) -> Option<mxconfig::Datetime>",
            "mxconfig::Value::Datetime(value)",
        ],
    );
    let dart = mxconfig_codegen::dart(&schema).unwrap();
    has(&dart, &["settings.setToml('seen', value)"]);
}

/// The text of the Dart string constant `name`, read back from `code`.
fn dart_const(code: &str, name: &str) -> String {
    let start = code
        .find(&format!("const String {name} ="))
        .expect("declared");
    let mut chars = code[start..].chars();
    let mut out = String::new();
    let mut quoted = false;
    while let Some(c) = chars.next() {
        match (quoted, c) {
            (false, ';') => return out,
            (_, '\'') => quoted = !quoted,
            (true, '\\') => match chars.next().expect("an escape") {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                c => out.push(c),
            },
            (true, c) => out.push(c),
            (false, _) => {}
        }
    }
    panic!("`{name}` is unterminated")
}

#[test]
fn the_dart_face_carries_the_schema_text_exactly() {
    let schema = keyed(concat!(
        r#"# it's ''' and """ with $HOME, ${x} and C:\dir"#,
        "\t.\n",
        r#"[keys.a]
type = "string"
label = "A"
description = "say \"hi\" for $5 in C:\\tmp"
"#,
    ));
    let dart = mxconfig_codegen::dart(&schema).unwrap();
    has(
        &dart,
        &[
            r#"'# it\'s \'\'\' and """ with \$HOME, \${x} and C:\\dir\t.\n'"#,
            r#"'description = "say \\"hi\\" for \$5 in C:\\\\tmp"\n';"#,
        ],
    );
    assert_eq!(dart_const(&dart, "schema"), schema.source());
}

#[test]
fn a_key_named_like_the_schema_constant_is_refused() {
    let schema = keyed("[keys.schema]\ntype = \"bool\"\nlabel = \"S\"\ndescription = \"d\"\n");
    assert!(mxconfig_codegen::dart(&schema).is_err());
}

/// A schema with managed and unmanaged keys and assets, in and out of a group.
const MIXED: &str = r#"
namespace = "org.mechanix.demo"
version = 1

[groups.display]
label = "Display"

[assets.layout]
path = "layout.yaml"
managed = true

[assets.theme]
path = "theme.css"

[keys.display.brightness]
type = "integer"
default = 50
label = "Brightness"
description = "Panel brightness."
managed = true

[keys.display.contrast]
type = "integer"
default = 50
label = "Contrast"
description = "Panel contrast."

[keys.display.mode]
type = "enum"
values = ["auto", "fixed"]
default = "auto"
label = "Mode"
description = "Brightness mode."
managed = true

[keys.lock]
type = "string"
label = "Lock"
description = "Lock policy."
managed = true

[keys.volume]
type = "integer"
default = 50
label = "Volume"
description = "Output volume."
"#;

/// Managed keys keep a typed getter but lose their writers, in a group or not.
#[test]
fn managed_keys_keep_only_their_getter_in_rust() {
    let rust = mxconfig_codegen::rust(&Schema::parse(MIXED).unwrap()).unwrap();
    has(
        &rust,
        &[
            "pub fn brightness(&self) -> i64 {",
            "pub fn mode(&self) -> DisplayMode {",
            "pub fn lock(&self) -> Option<String> {",
            // Unmanaged keys beside them keep every writer.
            "pub fn set_contrast(&self, value: i64)",
            "pub fn clear_contrast(&self)",
            "pub fn set_volume(&self, value: i64)",
            "pub fn clear_volume(&self)",
        ],
    );
    lacks(
        &rust,
        &[
            "set_brightness",
            "clear_brightness",
            "set_mode",
            "clear_mode",
            "set_lock",
            "clear_lock",
            "Edit::set(\"display.brightness\"",
            "Edit::remove(\"lock\")",
        ],
    );
}

/// Managed keys keep a typed getter but lose their writers in Dart too.
#[test]
fn managed_keys_keep_only_their_getter_in_dart() {
    let dart = mxconfig_codegen::dart(&Schema::parse(MIXED).unwrap()).unwrap();
    has(
        &dart,
        &[
            "int get brightness => settings.getInt('display.brightness') ?? 50;",
            "DisplayMode get mode =>",
            "String? get lock => settings.getString('lock');",
            "Future<void> setContrast(int value)",
            "Future<void> clearContrast()",
            "Future<void> setVolume(int value)",
        ],
    );
    lacks(
        &dart,
        &[
            "setBrightness",
            "clearBrightness",
            "setMode",
            "clearMode",
            "setLock",
            "clearLock",
            "settings.remove('lock')",
            "settings.setInt('display.brightness'",
        ],
    );
}

/// A managed key's changes are still typed, so apps can watch it.
#[test]
fn a_managed_key_still_has_a_typed_change() {
    let schema = Schema::parse(MIXED).unwrap();
    let rust = mxconfig_codegen::rust(&schema).unwrap();
    has(
        &rust,
        &[
            "    DisplayBrightness(i64),",
            "    Lock(Option<String>),",
            "\"display.brightness\" => DemoChange::DisplayBrightness(",
        ],
    );
    let dart = mxconfig_codegen::dart(&schema).unwrap();
    has(
        &dart,
        &[
            "final class DisplayBrightnessChange extends DemoChange<int> {",
            "'display.brightness' => DisplayBrightnessChange(",
        ],
    );
}

/// A managed asset keeps its getter and change, and loses every writer.
#[test]
fn a_managed_asset_keeps_only_its_getter() {
    let schema = Schema::parse(MIXED).unwrap();
    let rust = mxconfig_codegen::rust(&schema).unwrap();
    has(
        &rust,
        &[
            "pub fn asset_layout(&self) -> Option<mxconfig::Asset>",
            "\"layout\" => Some(DemoChange::AssetLayout),",
            "pub fn set_asset_theme(&self, bytes: &[u8])",
            "pub fn set_asset_theme_from(&self, file: std::os::fd::OwnedFd)",
            "pub fn clear_asset_theme(&self)",
        ],
    );
    lacks(
        &rust,
        &[
            "set_asset_layout",
            "set_asset_layout_from",
            "clear_asset_layout",
            "write_asset(\"layout\"",
            "write_asset_from(\"layout\"",
            "clear_asset(\"layout\")",
        ],
    );

    let dart = mxconfig_codegen::dart(&schema).unwrap();
    has(
        &dart,
        &[
            "Uint8List? assetLayout() => settings.asset('layout');",
            "import 'dart:typed_data';",
            "Future<void> setAssetTheme(Uint8List bytes)",
            "Future<void> clearAssetTheme()",
        ],
    );
    lacks(
        &dart,
        &[
            "setAssetLayout",
            "clearAssetLayout",
            "setAsset('layout'",
            "removeAsset('layout')",
        ],
    );
}

/// A managed key frees the writer names an unmanaged one would claim.
#[test]
fn a_managed_key_does_not_claim_writer_names() {
    let keys = "[keys.brightness]\ntype = \"integer\"\nlabel = \"B\"\ndescription = \"d\"\n{managed}\n\
                [keys.set_brightness]\ntype = \"integer\"\nlabel = \"S\"\ndescription = \"d\"\n";
    let managed = keyed(&keys.replace("{managed}", "managed = true"));
    let rust = mxconfig_codegen::rust(&managed).unwrap();
    has(&rust, &["pub fn set_brightness(&self) -> Option<i64> {"]);
    let dart = mxconfig_codegen::dart(&managed).unwrap();
    has(&dart, &["int? get setBrightness =>"]);

    // Unmanaged, the setter would clash with the other key's getter.
    let unmanaged = keyed(&keys.replace("{managed}", ""));
    assert!(mxconfig_codegen::rust(&unmanaged).is_err());
    assert!(mxconfig_codegen::dart(&unmanaged).is_err());
}
