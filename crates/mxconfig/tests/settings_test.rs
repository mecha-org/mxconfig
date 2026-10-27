mod common;

use common::Through;

use std::fs;
use std::path::Path;

use common::{SCHEMA, schema};
use mxconfig::Class;
use mxconfig::LayerKind;
use mxconfig::Schema;
use mxconfig::Settings;
use mxconfig::Value;
use mxconfig::host::Roots;
use mxconfig::{Access, Manifest};
use mxconfig::{Edit, Origin, Source};

fn open(dir: &Path) -> Settings {
    common::load_in(dir, schema()).unwrap()
}

fn file(dir: &Path) -> std::path::PathBuf {
    let ns_dir = dir.join("org.mechanix.demo");
    fs::create_dir_all(&ns_dir).unwrap();
    ns_dir.join("config.toml")
}

fn state(dir: &Path) -> std::path::PathBuf {
    file(dir).with_file_name("config.toml.state")
}

#[test]
fn defaults_come_from_the_schema() {
    let dir = tempfile::tempdir().unwrap();
    let settings = open(dir.path());

    assert_eq!(settings.describe().namespace(), "org.mechanix.demo");
    assert_eq!(settings.describe().version(), 2);
    assert_eq!(settings.revision(), 0);
    assert_eq!(
        settings.get("colors.accent"),
        Some(Value::String("#7aa2f7".into()))
    );
    assert_eq!(
        settings.get("colors.corner_radius"),
        Some(Value::Integer(8))
    );
    assert_eq!(
        settings.get("colors.mode"),
        Some(Value::String("dark".into()))
    );
    assert_eq!(settings.get("verbose"), Some(Value::Boolean(false)));
    assert_eq!(settings.resolved().iter().count(), 4);
    let diagnostics = settings.diagnostics();
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn set_validates_writes_and_reports_changes() {
    let dir = tempfile::tempdir().unwrap();
    let settings = open(dir.path());

    let changes = settings
        .set("colors.accent", Value::String("#ff0000".into()))
        .unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].key, "colors.accent");
    assert_eq!(changes[0].value, Some(Value::String("#ff0000".into())));
    assert_eq!(changes[0].revision, 1);
    assert_eq!(settings.revision(), 1);
    assert_eq!(
        settings.get("colors.accent"),
        Some(Value::String("#ff0000".into()))
    );

    // Wrong type, out of range, unknown key, bad enum member.
    assert!(settings.set("colors.accent", Value::Integer(1)).is_err());
    assert!(
        settings
            .set("colors.corner_radius", Value::Integer(99))
            .is_err()
    );
    assert!(settings.set("nope.nope", Value::Boolean(true)).is_err());
    assert!(
        settings
            .set("colors.mode", Value::String("sepia".into()))
            .is_err()
    );

    // Setting the same value again changes nothing.
    let again = settings
        .set("colors.accent", Value::String("#ff0000".into()))
        .unwrap();
    assert!(again.is_empty(), "{again:?}");
    assert_eq!(settings.revision(), 1);

    // Removing reports the fallback. A key without a default becomes unset.
    let changes = settings.remove("colors.accent").unwrap();
    assert_eq!(changes[0].value, Some(Value::String("#7aa2f7".into())));
    assert_eq!(
        settings.get("colors.accent"),
        Some(Value::String("#7aa2f7".into()))
    );

    settings
        .set("nickname", Value::String("aux".into()))
        .unwrap();
    let changes = settings.remove("nickname").unwrap();
    assert_eq!(changes[0].value, None);
    assert_eq!(settings.get("nickname"), None);
}

#[test]
fn unknown_keys_are_preserved_and_reported() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        file(dir.path()),
        "[colors]\nbogus = 1\naccent = \"#111111\"\n",
    )
    .unwrap();

    let settings = open(dir.path());
    assert_eq!(
        settings.get("colors.accent"),
        Some(Value::String("#111111".into()))
    );
    assert!(settings.get("colors.bogus").is_none());

    let diags = settings.diagnostics();
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert_eq!(diags[0].key(), Some("colors.bogus"));
    assert!(diags[0].message().contains("unknown key"));
    assert!(!diags[0].is_error(), "an unknown key is not fatal");
}

#[test]
fn bad_values_fall_back_to_defaults_and_report_errors() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        file(dir.path()),
        "[colors]\ncorner_radius = 999\naccent = 5\n",
    )
    .unwrap();

    let settings = open(dir.path());
    assert_eq!(
        settings.get("colors.corner_radius"),
        Some(Value::Integer(8))
    );
    assert_eq!(
        settings.get("colors.accent"),
        Some(Value::String("#7aa2f7".into()))
    );
    assert_eq!(
        settings
            .diagnostics()
            .iter()
            .filter(|d| d.is_error())
            .count(),
        2
    );
}

#[test]
fn prune_keeps_a_map_written_as_a_table() {
    const MAP: &str = r#"
namespace = "org.mechanix.demo"
version = 1

[keys.aliases]
type = "map"
label = "Aliases"
description = "Short names for commands."
"#;
    let dir = tempfile::tempdir().unwrap();
    let text = "[aliases]\nll = \"ls -l\"\n\n[stray]\nx = 1\n";
    fs::write(file(dir.path()), text).unwrap();

    let settings = common::load_in(dir.path(), Schema::parse(MAP).unwrap()).unwrap();
    assert_eq!(settings.prune().unwrap(), vec!["stray.x".to_string()]);
    assert!(settings.get("aliases").is_some());
}

#[test]
fn prune_drops_keys_the_schema_does_not_define() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        file(dir.path()),
        "[colors]\nbogus = 1\naccent = \"#222222\"\n",
    )
    .unwrap();

    let settings = open(dir.path());
    assert_eq!(settings.prune().unwrap(), vec!["colors.bogus".to_string()]);
    assert_eq!(
        settings.get("colors.accent"),
        Some(Value::String("#222222".into()))
    );
    assert!(
        !fs::read_to_string(file(dir.path()))
            .unwrap()
            .contains("bogus")
    );
}

#[test]
fn a_manifest_gates_the_handle() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = Manifest::parse(
        "client = \"org.mechanix.demo\"\n\n[[settings]]\nnamespace = \"org.mechanix.demo\"\naccess = \"read\"\n",
    )
    .unwrap();

    assert!(
        open(dir.path())
            .with_manifest(&manifest, Access::Read)
            .is_ok()
    );
    assert!(
        open(dir.path())
            .with_manifest(&manifest, Access::ReadWrite)
            .is_err()
    );
}

#[test]
fn open_with_roots_discovers_an_installed_schema() {
    let tmp = tempfile::tempdir().unwrap();
    let roots = Roots::under(tmp.path());
    common::install(&roots, "org.mechanix.demo", SCHEMA);

    // Discovery finds the schema and user layer.
    let settings = common::open(&Source::with_roots(roots.clone()), "org.mechanix.demo").unwrap();
    assert_eq!(settings.describe().class(), Class::App);
    assert_eq!(
        settings.get("colors.accent"),
        Some(Value::String("#7aa2f7".into()))
    );
    settings
        .set("colors.accent", Value::String("#123456".into()))
        .unwrap();
    assert!(
        roots
            .user_dir
            .as_ref()
            .unwrap()
            .join("org.mechanix.demo/config.toml")
            .is_file()
    );

    // Opening an uninstalled namespace fails and names the schema directory.
    let missing = common::open(&Source::with_roots(roots.clone()), "org.other.thing").unwrap_err();
    assert!(missing.to_string().contains("org.other.thing"), "{missing}");
}

/// `apply` writes several keys in one revision.
#[test]
fn apply_writes_several_keys_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let settings = open(dir.path());

    let changes = settings
        .apply(&[
            Edit::set("colors.accent", Value::String("#123456".into())),
            Edit::set("colors.corner_radius", Value::Integer(20)),
        ])
        .unwrap();

    assert!(changes.iter().any(|c| c.key == "colors.accent"));
    assert!(changes.iter().any(|c| c.key == "colors.corner_radius"));
    assert_eq!(
        settings.get("colors.accent"),
        Some(Value::String("#123456".into()))
    );
    assert_eq!(
        settings.get("colors.corner_radius"),
        Some(Value::Integer(20))
    );
    assert_eq!(settings.revision(), 1, "one batch, one revision");
}

/// A batch with an invalid edit writes nothing at all.
#[test]
fn a_failed_batch_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let settings = open(dir.path());
    settings
        .set("colors.accent", Value::String("#111111".into()))
        .unwrap();
    let before = fs::read(file(dir.path())).unwrap();

    let result = settings.apply(&[
        Edit::set("colors.accent", Value::String("#222222".into())),
        Edit::set("colors.corner_radius", Value::String("nope".into())),
    ]);

    assert!(result.is_err(), "a bad edit fails the batch");
    assert_eq!(
        fs::read(file(dir.path())).unwrap(),
        before,
        "nothing written"
    );
    assert_eq!(
        settings.get("colors.accent"),
        Some(Value::String("#111111".into())),
        "the good edit was not applied either"
    );
}

/// A batch can set and remove together.
#[test]
fn a_batch_sets_and_removes() {
    let dir = tempfile::tempdir().unwrap();
    let settings = open(dir.path());
    settings
        .set("colors.accent", Value::String("#111111".into()))
        .unwrap();

    settings
        .apply(&[
            Edit::set("colors.corner_radius", Value::Integer(30)),
            Edit::remove("colors.accent"),
        ])
        .unwrap();

    assert_eq!(
        settings.get("colors.corner_radius"),
        Some(Value::Integer(30))
    );
    assert_eq!(
        settings.get("colors.accent"),
        Some(Value::String("#7aa2f7".into())),
        "removing falls back to the schema default"
    );
}

/// Later edits win when a batch repeats a key.
#[test]
fn a_batch_applies_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let settings = open(dir.path());
    settings
        .apply(&[
            Edit::set("colors.accent", Value::String("#aaaaaa".into())),
            Edit::set("colors.accent", Value::String("#bbbbbb".into())),
        ])
        .unwrap();
    assert_eq!(
        settings.get("colors.accent"),
        Some(Value::String("#bbbbbb".into()))
    );
}

/// An empty batch does nothing.
#[test]
fn an_empty_batch_is_a_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let settings = open(dir.path());
    let changes = settings.apply(&[]).unwrap();
    assert!(changes.is_empty(), "{changes:?}");
    assert_eq!(settings.revision(), 0);
}

/// A schema with an asset, for the asset tests.
const ASSET_SCHEMA: &str = r#"
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

/// Roots under `tmp` with `schema` installed.
fn asset_roots(tmp: &std::path::Path, schema: &str) -> Roots {
    let roots = Roots::under(tmp);
    common::install(&roots, "org.mechanix.demo", schema);
    roots
}

/// Write asset `layout`'s file into a layer root.
fn put_asset(root: &std::path::Path, text: &str) {
    common::put(&root.join("org.mechanix.demo/ui/layout.yaml"), text);
}

/// Assets resolve across layers like values. The highest layer wins.
#[test]
fn an_asset_resolves_across_layers() {
    let tmp = tempfile::tempdir().unwrap();
    let roots = asset_roots(tmp.path(), ASSET_SCHEMA);
    put_asset(roots.vendor_dir.as_ref().unwrap(), "vendor");
    put_asset(roots.admin_dir.as_ref().unwrap(), "admin");

    let settings = common::open(&Source::with_roots(roots.clone()), "org.mechanix.demo").unwrap();
    let asset = settings.asset("layout").expect("an asset resolves");
    assert_eq!(asset.text().unwrap(), "admin");
    assert!(matches!(
        asset.origin,
        Origin::File {
            kind: LayerKind::Admin,
            ..
        }
    ));

    // The user copy wins. Removing it falls back to the admin copy.
    settings.set_asset("layout", b"user").unwrap();
    let asset = settings.asset("layout").unwrap();
    assert_eq!(asset.text().unwrap(), "user");
    assert!(matches!(
        asset.origin,
        Origin::File {
            kind: LayerKind::User,
            ..
        }
    ));
    settings.remove_asset("layout").unwrap();
    assert_eq!(settings.asset("layout").unwrap().text().unwrap(), "admin");
}

/// An env var naming no file, and a directory where a file belongs, are skipped with a warning.
#[test]
fn an_unusable_asset_file_lets_the_layers_apply() {
    const VAR: &str = "MXCONFIG_TEST_MISSING_LAYOUT";
    let tmp = tempfile::tempdir().unwrap();
    let schema = ASSET_SCHEMA.replace(
        "path = \"ui/layout.yaml\"",
        &format!("path = \"ui/layout.yaml\"\nenv = \"{VAR}\""),
    );
    let roots = asset_roots(tmp.path(), &schema);
    put_asset(roots.admin_dir.as_ref().unwrap(), "admin");
    let user = roots.user_dir.as_ref().unwrap();
    fs::create_dir_all(user.join("org.mechanix.demo/ui/layout.yaml")).unwrap();
    // SAFETY: no other test reads this variable.
    unsafe { std::env::set_var(VAR, tmp.path().join("missing.yaml")) };

    let settings = common::open(&Source::with_roots(roots), "org.mechanix.demo").unwrap();
    assert_eq!(settings.asset("layout").unwrap().text().unwrap(), "admin");
    let diagnostics = settings.diagnostics();
    let says = |what: &str| diagnostics.iter().any(|d| d.message().contains(what));
    assert!(says("which is not a file"), "{diagnostics:?}");
    assert!(says("is a directory"), "{diagnostics:?}");
}

/// A `rename-asset` migration moves the user's asset file.
#[test]
fn an_asset_migrates_with_its_namespace() {
    const SCHEMA: &str = r#"
namespace = "org.mechanix.demo"
version = 2

[assets.layout]
path = "ui/layout.kdl"

[keys.brightness]
type = "integer"
default = 50
label = "Brightness"
description = "Panel brightness."

[[migration]]
from = 1
to = 2
ops = [{ op = "rename-asset", from = "ui/layout.yaml", to = "ui/layout.kdl" }]
"#;
    let dir = tempfile::tempdir().unwrap();
    let ns_dir = dir.path().join("org.mechanix.demo");
    fs::create_dir_all(ns_dir.join("ui")).unwrap();
    fs::write(file(dir.path()), "brightness = 30\n").unwrap();
    fs::write(state(dir.path()), "version = 1\n").unwrap();
    fs::write(ns_dir.join("ui/layout.yaml"), "old-layout").unwrap();

    let settings = common::load_in(dir.path(), Schema::parse(SCHEMA).unwrap()).unwrap();

    assert_eq!(settings.describe().version(), 2);
    assert!(!ns_dir.join("ui/layout.yaml").exists(), "moved away");
    assert_eq!(
        settings.asset("layout").unwrap().text().unwrap(),
        "old-layout"
    );
}

/// A broken file reads as its last good copy, and a write over it keeps it aside.
#[test]
fn a_broken_file_reads_as_its_last_good_copy_until_written_over() {
    let dir = tempfile::tempdir().unwrap();
    let settings = open(dir.path());
    settings
        .set("colors.accent", Value::String("#123456".into()))
        .unwrap();
    let broken = "[colors\naccent = ";
    fs::write(file(dir.path()), broken).unwrap();

    settings.reload();
    let accent = Value::String("#123456".into());
    assert_eq!(settings.get("colors.accent"), Some(accent));
    let diags = settings.diagnostics();
    assert!(
        diags.iter().any(|d| d.message().contains("last good")),
        "{diags:?}"
    );
    let text = fs::read_to_string(file(dir.path())).unwrap();
    assert_eq!(text, broken, "reading leaves the file alone");

    // A write saves it aside and starts from the last good copy.
    settings
        .set("colors.corner_radius", Value::Integer(4))
        .unwrap();
    let aside = file(dir.path()).with_file_name("config.toml.broken");
    assert_eq!(
        fs::read_to_string(aside).unwrap(),
        broken,
        "kept to recover from"
    );
    let accent = Value::String("#123456".into());
    assert_eq!(
        settings.get("colors.accent"),
        Some(accent),
        "the last good value"
    );
    assert_eq!(
        settings.get("colors.corner_radius"),
        Some(Value::Integer(4))
    );
}
