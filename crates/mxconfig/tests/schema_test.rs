mod common;

use common::schema;
use mxconfig::Class;
use mxconfig::{KeyType, Schema};

#[test]
fn class_is_derived_from_the_id() {
    assert_eq!(Class::of("org.mechanix.demo").unwrap(), Class::App);
    assert_eq!(
        Class::of("org.mechanix.desktop.theme").unwrap(),
        Class::Desktop
    );
    assert_eq!(
        Class::of("org.mechanix.privileged.display").unwrap(),
        Class::Privileged
    );
    assert!(Class::of("org").is_err(), "a single label is not enough");
    assert!(
        Class::of("org.mechanix.desktop").is_err(),
        "reserved label last"
    );
    assert!(Class::of("desktop.theme").is_err(), "reserved label first");
    assert!(
        Class::of("org.mechanix.desktop.a.desktop.b").is_err(),
        "repeated"
    );
    assert!(
        Class::of("org.1bad.x").is_err(),
        "label starts with a digit"
    );
}

#[test]
fn parse_reads_groups_types_and_migrations() {
    let schema = schema();
    assert_eq!(schema.namespace(), "org.mechanix.demo");
    assert_eq!(schema.class(), Class::App);
    assert_eq!(schema.version(), 2);
    assert_eq!(schema.groups().len(), 1);
    assert_eq!(schema.keys().len(), 5);
    assert_eq!(schema.migrations().len(), 1);

    let accent = schema.key("colors.accent").unwrap();
    assert_eq!(accent.ty, KeyType::String);
    assert_eq!(accent.group, "colors");
    assert_eq!(accent.label, "Accent");

    let mode = schema.key("colors.mode").unwrap();
    assert_eq!(mode.ty, KeyType::Enum(vec!["light".into(), "dark".into()]));
    assert_eq!(mode.label, "Mode");

    let radius = schema.key("colors.corner_radius").unwrap();
    assert_eq!(radius.min, Some(0.0));
    assert_eq!(radius.max, Some(64.0));

    let verbose = schema.key("verbose").unwrap();
    assert_eq!(verbose.group, "");

    // Grouped keys come before ungrouped ones, groups in `order`.
    let keys = schema.keys();
    assert_eq!(keys[0].group, "colors");
    assert_eq!(keys.last().unwrap().group, "");
}

#[test]
fn validate_accepts_the_fixture() {
    let diags = schema().validate();
    assert!(diags.is_empty(), "{diags:?}");
}

/// An edge past the schema's version is no path, as migrating would find.
#[test]
fn validate_refuses_an_edge_past_the_version() {
    let src = r#"
namespace = "org.mechanix.demo"
version = 2

[[migration]]
from = 1
to = 3
ops = []
"#;
    let diags = Schema::parse(src).unwrap().validate();
    let messages: Vec<&str> = diags.iter().map(mxconfig::Diagnostic::message).collect();
    assert!(
        messages.iter().any(|m| m.contains("no migration from v1")),
        "{messages:?}"
    );
}

#[test]
fn validate_reports_a_missing_description_and_a_bad_default() {
    let src = r#"
namespace = "org.mechanix.demo"
version = 1

[keys.n]
type = "integer"
default = "not a number"
label = "N"
"#;
    let diags = Schema::parse(src).unwrap().validate();
    let messages: Vec<&str> = diags.iter().map(mxconfig::Diagnostic::message).collect();
    assert!(
        messages.iter().any(|m| m.contains("no description")),
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| m.contains("but the schema declares")),
        "{messages:?}"
    );
}

#[test]
fn unknown_types_and_bad_ids_are_errors() {
    assert!(Schema::parse("namespace = \"org.x.y\"\nversion = 1\n").is_ok());
    assert!(Schema::parse("namespace = \"nope\"\nversion = 1\n").is_err());
    assert!(
        Schema::parse("namespace = \"org.x.y\"\n").is_err(),
        "version required"
    );

    let bad_type = r#"
namespace = "org.x.y"
version = 1

[keys.k]
type = "integer"
label = "K"
description = "d"

[keys.w]
type = "wat"
label = "W"
description = "d"
"#;
    assert!(Schema::parse(bad_type).is_err());
}

#[test]
fn an_asset_path_is_relative_and_confined() {
    let src = |path: &str| {
        format!("namespace = \"org.mechanix.demo\"\nversion = 1\n\n[assets.a]\npath = \"{path}\"\n")
    };
    assert!(Schema::parse(&src("ui/x.yaml")).is_ok());
    assert!(Schema::parse(&src("/abs/x.yaml")).is_err(), "absolute");
    assert!(Schema::parse(&src("../x.yaml")).is_err(), "parent dir");
}

#[test]
fn array_and_map_element_types_are_parsed() {
    let src = r#"
namespace = "org.mechanix.demo"
version = 1

[keys.recent]
type = "array"
element = "string"
label = "Recent"
description = "d"

[keys.limits]
type = "map"
element = "integer"
label = "Limits"
description = "d"
"#;
    let schema = Schema::parse(src).unwrap();
    assert_eq!(
        schema.key("recent").unwrap().ty,
        KeyType::Array(Some(Box::new(KeyType::String)))
    );
    assert_eq!(
        schema.key("limits").unwrap().ty,
        KeyType::Map(Some(Box::new(KeyType::Integer)))
    );
}

#[test]
fn validate_reports_an_element_type_mismatch() {
    let src = r#"
namespace = "org.mechanix.demo"
version = 1

[keys.recent]
type = "array"
element = "integer"
default = ["a"]
label = "Recent"
description = "d"
"#;
    let diags = Schema::parse(src).unwrap().validate();
    assert!(
        diags.iter().any(|d| d.message().contains("element type")),
        "{diags:?}"
    );
}

#[test]
fn env_vars_must_be_valid_and_declared_once() {
    let schema = |key_env: &str, asset_env: &str| {
        format!(
            r#"
namespace = "org.mechanix.demo"
version = 1

[assets.layout]
path = "layout.yaml"
env = "{asset_env}"

[keys.scale]
type = "float"
env = "{key_env}"
label = "Scale"
description = "d"
"#
        )
    };
    let parsed = Schema::parse(&schema("DEMO_SCALE", "DEMO_LAYOUT")).unwrap();
    assert_eq!(
        parsed.key("scale").unwrap().env.as_deref(),
        Some("DEMO_SCALE")
    );

    let err = Schema::parse(&schema("DEMO-SCALE", "DEMO_LAYOUT")).unwrap_err();
    assert!(err.to_string().contains("invalid env var"), "{err}");
    let err = Schema::parse(&schema("DEMO_X", "DEMO_X")).unwrap_err();
    assert!(err.to_string().contains("declared twice"), "{err}");
}

#[test]
fn only_an_app_chooses_its_backend() {
    let app = |backend: &str| {
        format!("namespace = \"org.mechanix.demo\"\nversion = 1\nbackend = \"{backend}\"\n")
    };
    let parsed = Schema::parse(&app("daemon-only")).unwrap();
    assert_eq!(parsed.backend(), Some(mxconfig_schema::Backend::DaemonOnly));
    assert!(Schema::parse(&app("sideways")).is_err());
    let shared = "namespace = \"org.mechanix.desktop.demo\"\nversion = 1\nbackend = \"local\"\n";
    assert!(Schema::parse(shared).is_err());
}

#[test]
fn a_retired_name_cannot_be_declared_again() {
    let base = "namespace = \"org.example.app\"\nversion = 1\n";
    let key = "[keys.eq]\ntype = \"string\"\nlabel = \"L\"\ndescription = \"d\"\n";
    let asset = "[assets.cache]\npath = \"cache.bin\"\n";
    let retired = "[retired]\nkeys = [\"eq\"]\nassets = [\"cache.bin\"]\n";
    assert!(Schema::parse(&format!("{base}{retired}")).is_ok());
    let key_again = Schema::parse(&format!("{base}{key}{retired}")).unwrap_err();
    assert!(key_again.to_string().contains("retired"), "{key_again}");
    assert!(Schema::parse(&format!("{base}{asset}{retired}")).is_err());
    assert!(Schema::parse(&format!("{base}[retired]\nassets = [\"../x\"]\n")).is_err());
}

#[test]
fn migrations_have_no_add_or_remove() {
    let edge = |op: &str| {
        format!(
            "namespace = \"org.example.app\"\nversion = 2\n[[migration]]\nfrom = 1\nto = 2\nops = [{op}]\n"
        )
    };
    for op in [
        r#"{ op = "add", key = "a", value = 1 }"#,
        r#"{ op = "remove", key = "a" }"#,
        r#"{ op = "remove-asset", path = "a" }"#,
    ] {
        assert!(Schema::parse(&edge(op)).is_err(), "{op}");
    }
}

/// v1 has `a` and `b`. v2 renames `a` to `c` and keeps `b`.
fn renaming(version: u32) -> Schema {
    let key = |name: &str| {
        format!("[keys.{name}]\ntype = \"integer\"\nlabel = \"L\"\ndescription = \"d\"\n")
    };
    let head = format!("namespace = \"org.example.app\"\nversion = {version}\n");
    let text = match version {
        1 => head + &key("a") + &key("b"),
        _ => {
            head + &key("c")
                + &key("b")
                + "[[migration]]\nfrom = 1\nto = 2\nops = [{ op = \"rename\", from = \"a\", to = \"c\" }]\n"
        }
    };
    Schema::parse(&text).unwrap()
}

#[test]
fn another_version_reads_the_same_only_for_untouched_names() {
    let (v1, v2) = (renaming(1), renaming(2));
    assert!(!v1.reads_same(&v2), "`a` was renamed");
    assert!(!v2.reads_same(&v1), "`c` was renamed");
    assert!(v1.trimmed(&["b"]).unwrap().reads_same(&v2));
    assert!(v2.trimmed(&["b"]).unwrap().reads_same(&v1));
    // Without the edges between them, nothing can be said.
    assert!(!v2.trimmed(&["b"]).unwrap().reads_through(1, &[]));
}

#[test]
fn a_trimmed_schema_keeps_only_the_listed_keys() {
    let trimmed = schema().trimmed(&["colors.accent", "verbose"]).unwrap();
    let paths: Vec<&str> = trimmed.keys().iter().map(|k| k.path.as_str()).collect();
    assert_eq!(paths, ["colors.accent", "verbose"]);
    assert_eq!(trimmed.version(), schema().version());
    assert_eq!(trimmed.migrations(), schema().migrations());
    let alone = schema().trimmed(&["verbose"]).unwrap();
    assert!(alone.groups().is_empty(), "a group without keys goes");
    assert!(schema().trimmed(&["nope"]).is_err());
}
