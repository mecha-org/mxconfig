//! Tests the bridged API from Rust, the way Dart uses it.

use crate::api::source::{MxArg, MxArgKind, MxOutcome, MxOutcomeKind};
use crate::api::source::{MxSource, MxTransport};
use crate::api::{Change, MxSettings, MxconfigException};
use tempfile::TempDir;

const SCHEMA: &str = r##"
namespace = "org.mechanix.demo"
version = 1

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

[keys.colors.scale]
type = "float"
default = 1.0
label = "Scale"
description = "Interface scale factor."

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
"##;

/// Open `schema` over `dir` through a source.
fn open(dir: &TempDir, schema: &str) -> (MxSource, MxSettings) {
    let source = MxSource::with_user_dir(dir.path().display().to_string());
    let settings = source.open_schema(schema.to_string()).expect("opens");
    (source, settings)
}

/// Write the user layer's file under `dir`, as an outside editor would.
fn write_user(dir: &TempDir, toml: &str) {
    let path = dir.path().join("org.mechanix.demo");
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join("config.toml"), toml).unwrap();
}

#[test]
fn reads_defaults_and_writes_typed_values() {
    let dir = TempDir::new().unwrap();
    let (source, settings) = open(&dir, SCHEMA);

    assert_eq!(settings.namespace(), "org.mechanix.demo");
    assert_eq!(settings.revision(), 0);
    assert!(!settings.from_daemon());

    assert_eq!(
        settings.get_string("colors.accent".into()),
        Some("#7aa2f7".to_string())
    );
    assert_eq!(settings.get_int("colors.corner_radius".into()), Some(8));
    assert_eq!(settings.get_double("colors.scale".into()), Some(1.0));
    assert_eq!(settings.get_bool("verbose".into()), Some(false));
    assert_eq!(settings.get_string("nickname".into()), None);
    assert_eq!(
        settings.get_toml("colors.mode".into()),
        Some("\"dark\"".to_string())
    );

    // A local write finishes immediately.
    let changes = landed(
        &source,
        settings.set_string("colors.accent".into(), "#ff0000".into()),
    );
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].key, "colors.accent");
    assert_eq!(changes[0].value, Some("\"#ff0000\"".to_string()));
    assert_eq!(
        settings.get_string("colors.accent".into()),
        Some("#ff0000".to_string())
    );

    landed(&source, settings.set_int("colors.corner_radius".into(), 20));
    landed(&source, settings.set_double("colors.scale".into(), 1.5));
    landed(&source, settings.set_bool("verbose".into(), true));
    assert_eq!(settings.get_int("colors.corner_radius".into()), Some(20));
    assert_eq!(settings.get_double("colors.scale".into()), Some(1.5));
    assert_eq!(settings.get_bool("verbose".into()), Some(true));

    // Arrays and maps are passed as TOML text.
    assert!(settings.get_toml("colors.mode".into()).is_some());
}

/// The changes a local write made.
fn landed(source: &MxSource, ticket: Result<u64, MxconfigException>) -> Vec<Change> {
    let ticket = ticket.unwrap();
    let outcomes = source.outcomes();
    let written = outcomes
        .iter()
        .find(|o| matches!(o.kind, MxOutcomeKind::Written) && o.ticket == ticket);
    let error = &written.expect("a local write lands at once").error;
    assert!(error.is_none(), "{error:?}");
    changed(outcomes)
}

/// The value changes in `outcomes`.
fn changed(outcomes: Vec<MxOutcome>) -> Vec<Change> {
    outcomes.into_iter().filter_map(|o| o.change).collect()
}

#[test]
fn refuses_bad_values_and_reports_changes_on_removal() {
    let dir = TempDir::new().unwrap();
    let (source, settings) = open(&dir, SCHEMA);

    assert!(settings.set_int("colors.corner_radius".into(), 99).is_err());
    assert!(settings.set_int("colors.accent".into(), 5).is_err());
    assert!(settings.set_bool("nope.nope".into(), true).is_err());
    assert!(
        settings
            .set_string("colors.mode".into(), "sepia".into())
            .is_err()
    );

    landed(
        &source,
        settings.set_string("colors.accent".into(), "#ff0000".into()),
    );
    let removed = landed(&source, settings.remove("colors.accent".into()));
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].value, Some("\"#7aa2f7\"".to_string()));
    assert_eq!(
        settings.get_toml("colors.accent".into()).as_deref(),
        Some("\"#7aa2f7\"")
    );
}

#[test]
fn watch_dirs_cover_the_user_layer_and_a_rescan_reports_external_edits() {
    let dir = TempDir::new().unwrap();
    let (source, settings) = open(&dir, SCHEMA);

    let dirs = source.take_watch_dirs().unwrap();
    let expected = dir.path().join("org.mechanix.demo").display().to_string();
    assert!(
        dirs.iter().any(|d| d.trim_end_matches('/') == expected),
        "{dirs:?}"
    );

    // An outside edit is reported after a rescan.
    write_user(&dir, "[colors]\naccent = \"#00ff00\"\n");
    source.rescan();
    let changes = changed(source.outcomes());
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].key, "colors.accent");
    assert_eq!(changes[0].value, Some("\"#00ff00\"".to_string()));
    assert_eq!(
        settings.get_string("colors.accent".into()),
        Some("#00ff00".to_string())
    );

    source.rescan();
    assert!(
        changed(source.outcomes()).is_empty(),
        "nothing changed since"
    );
}

#[test]
fn describes_the_schema_for_a_settings_ui() {
    let dir = TempDir::new().unwrap();
    let (_source, settings) = open(&dir, SCHEMA);
    let describe = settings.describe();

    assert_eq!(describe.namespace, "org.mechanix.demo");
    assert_eq!(describe.class_name, "app");
    assert_eq!(describe.version, 1);
    assert_eq!(describe.groups.len(), 1);
    assert_eq!(describe.groups[0].id, "colors");
    assert_eq!(describe.groups[0].label, "Colors");
    assert_eq!(describe.keys.len(), 5);

    let mode = describe
        .keys
        .iter()
        .find(|key| key.key == "colors.mode")
        .expect("mode is described");
    assert_eq!(mode.ty, "enum");
    assert_eq!(mode.values, vec!["light".to_string(), "dark".to_string()]);
    assert_eq!(mode.default_toml.as_deref(), Some("\"dark\""));
    assert_eq!(mode.label, "Mode");
    assert_ne!(mode.description, "");

    let radius = describe
        .keys
        .iter()
        .find(|key| key.key == "colors.corner_radius")
        .expect("radius is described");
    assert_eq!(radius.min, Some(0.0));
    assert_eq!(radius.max, Some(64.0));
}

#[test]
fn reports_an_unknown_key_without_failing() {
    let dir = TempDir::new().unwrap();
    write_user(&dir, "[colors]\nbogus = 1\naccent = \"#111111\"\n");

    let (_source, settings) = open(&dir, SCHEMA);
    assert_eq!(
        settings.get_string("colors.accent".into()),
        Some("#111111".to_string())
    );

    let diagnostics = settings.diagnostics();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].severity, "warning");
    assert_eq!(diagnostics[0].key.as_deref(), Some("colors.bogus"));
    assert!(diagnostics[0].message.contains("unknown key"));
}

fn arg(kind: MxArgKind) -> MxArg {
    MxArg {
        kind,
        text: String::new(),
        number: 0,
        bytes: Vec::new(),
        items: Vec::new(),
    }
}

fn text(text: &str) -> MxArg {
    MxArg {
        text: text.into(),
        ..arg(MxArgKind::Str)
    }
}

fn items(kind: MxArgKind, text: &str, items: Vec<MxArg>) -> MxArg {
    MxArg {
        text: text.into(),
        items,
        ..arg(kind)
    }
}

/// A `Subscribe` reply with `values` as TOML text and `(id, bytes)` assets.
fn subscribed(schema: &str, values: &[(&str, &str)], assets: &[(&str, &[u8])]) -> Vec<MxArg> {
    let value = |(key, toml): &(&str, &str)| {
        let toml = items(MxArgKind::Variant, "", vec![text(toml)]);
        items(MxArgKind::Struct, "", vec![text(key), toml])
    };
    let asset = |(id, bytes): &(&str, &[u8])| {
        let fd = MxArg {
            bytes: bytes.to_vec(),
            ..arg(MxArgKind::Fd)
        };
        items(
            MxArgKind::Struct,
            "",
            vec![text(id), text("/daemon/asset"), fd],
        )
    };
    // The subscription, which Dart keeps open as descriptor 9.
    let kept = MxArg {
        number: 9,
        ..arg(MxArgKind::Kept)
    };
    vec![
        kept,
        text(schema),
        MxArg {
            number: 1,
            ..arg(MxArgKind::U64)
        },
        items(MxArgKind::Dict, "", values.iter().map(value).collect()),
        items(
            MxArgKind::Array,
            "(ssh)",
            assets.iter().map(asset).collect(),
        ),
    ]
}

/// Open through a daemon that answers `Subscribe` with `reply`.
fn served(dir: &TempDir, reply: Vec<MxArg>) -> (MxSource, MxOutcome) {
    // No schema here, so the open falls back to the daemon.
    let source = MxSource::with_user_dir(dir.path().display().to_string());
    let transport = source.transport();
    transport.reach("session".into()).unwrap();
    let ticket = source.request("org.mechanix.demo".into());
    let calls = transport.take_calls("session".into()).unwrap();
    let [owner, call] = &*calls else {
        panic!("{calls:?}")
    };
    assert_eq!(
        (owner.member.as_str(), call.member.as_str()),
        ("GetNameOwner", "Subscribe")
    );
    transport
        .reply("session".into(), owner.id, vec![text(":1.1")])
        .unwrap();
    transport.reply("session".into(), call.id, reply).unwrap();
    let outcome = source.outcomes().into_iter().find(|o| o.ticket == ticket);
    (source, outcome.expect("the open finished"))
}

/// The subscription Dart keeps is released once its handle goes, for Dart to close.
#[test]
fn a_kept_subscription_is_released_with_its_handle() {
    let dir = TempDir::new().unwrap();
    let (source, opened) = served(&dir, subscribed(SCHEMA, &[], &[]));
    let settings = opened.settings.expect("opened");
    assert!(settings.from_daemon());
    assert_eq!(
        source.transport().take_released("session".into()).unwrap(),
        Vec::<u64>::new()
    );
    drop(settings);
    assert_eq!(
        source.transport().take_released("session".into()).unwrap(),
        [9]
    );
}

#[test]
fn a_snapshot_value_dart_cannot_read_fails_the_open() {
    let dir = TempDir::new().unwrap();
    let reply = subscribed(SCHEMA, &[("colors.accent", "not toml")], &[]);
    let (_source, outcome) = served(&dir, reply);
    assert!(outcome.settings.is_none(), "not an open with the key unset");
    assert!(outcome.error.is_some());
}

/// Asset bytes passed from Dart are readable as the asset.
#[test]
fn a_served_asset_reads_the_bytes_dart_handed_over() {
    const WITH_ASSET: &str = "namespace = \"org.mechanix.demo\"\nversion = 1\n\n[assets.layout]\npath = \"layout.yaml\"\n";
    let dir = TempDir::new().unwrap();
    let reply = subscribed(WITH_ASSET, &[], &[("layout", b"hello")]);
    let (source, opened) = served(&dir, reply);
    let settings = opened.settings.expect("opened");
    assert_eq!(
        settings.asset("layout".into()).as_deref(),
        Some(&b"hello"[..])
    );

    let rule = MxTransport::rules()
        .into_iter()
        .find(|rule| rule.member == "AssetChanged")
        .unwrap();
    let asset_changed = |file: Vec<MxArg>| {
        let args = vec![
            text("org.mechanix.demo"),
            text("layout"),
            items(MxArgKind::Array, "(sh)", file),
        ];
        let (interface, member) = (rule.interface.clone(), rule.member.clone());
        source
            .transport()
            .signal("session".into(), ":1.1".into(), interface, member, args)
            .unwrap();
    };
    let file = MxArg {
        bytes: b"bye".to_vec(),
        ..arg(MxArgKind::Fd)
    };
    asset_changed(vec![items(MxArgKind::Struct, "", vec![text("/x"), file])]);
    let outcomes = source.outcomes();
    assert!(matches!(&*outcomes, [o] if matches!(o.kind, MxOutcomeKind::Asset)));
    assert_eq!(
        settings.asset("layout".into()).as_deref(),
        Some(&b"bye"[..])
    );

    asset_changed(Vec::new());
    assert_eq!(settings.asset("layout".into()), None);
}

#[test]
fn an_embedded_schema_opens_when_none_is_installed() {
    let dir = TempDir::new().unwrap();
    let source = MxSource::with_user_dir(dir.path().display().to_string());
    source.embed("org.mechanix.demo".into(), SCHEMA.into());
    let ticket = source.request("org.mechanix.demo".into());
    let opened = source.outcomes().into_iter().find(|o| o.ticket == ticket);
    let settings = opened.and_then(|o| o.settings).expect("opened");
    assert_eq!(
        settings.get_string("colors.accent".into()),
        Some("#7aa2f7".to_string())
    );
}

#[test]
fn the_owner_rule_carries_the_daemon_name_as_arg0() {
    let rules = MxTransport::rules();
    let owner = rules.iter().find(|rule| rule.member == "NameOwnerChanged");
    assert_eq!(
        owner.and_then(|rule| rule.arg0.as_deref()),
        Some("org.mechanix.MxConfig")
    );
    assert!(rules.iter().any(|rule| rule.arg0.is_none()));
}

/// A schema with managed keys and a managed asset beside unmanaged ones.
const MANAGED: &str = r#"
namespace = "org.mechanix.demo"
version = 1

[assets.layout]
path = "layout.yaml"
managed = true

[assets.theme]
path = "theme.css"

[keys.brightness]
type = "integer"
default = 50
label = "Brightness"
description = "Panel brightness."
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

#[test]
fn describe_marks_managed_keys() {
    let dir = TempDir::new().unwrap();
    let (_source, settings) = open(&dir, MANAGED);
    let describe = settings.describe();
    let managed = |key: &str| {
        let info = describe.keys.iter().find(|info| info.key == key);
        info.expect("described").managed
    };
    assert!(managed("brightness"));
    assert!(managed("lock"));
    assert!(!managed("volume"));

    // The plain schema has no managed key at all.
    let (_source, plain) = open(&dir, SCHEMA);
    assert!(plain.describe().keys.iter().all(|info| !info.managed));
}

#[test]
fn every_setter_fails_for_a_managed_key() {
    let dir = TempDir::new().unwrap();
    let (source, settings) = open(&dir, MANAGED);
    let refusals = [
        settings.set_int("brightness".into(), 1),
        settings.set_toml("brightness".into(), "1".into()),
        settings.remove("brightness".into()),
        settings.set_string("lock".into(), "on".into()),
        settings.remove("lock".into()),
    ];
    for refused in refusals {
        let e = refused.expect_err("a managed key is never written");
        assert!(e.message.contains("managed"), "{}", e.message);
    }
    assert!(source.outcomes().is_empty());
    assert!(!dir.path().join("org.mechanix.demo/config.toml").exists());
    assert_eq!(settings.get_int("brightness".into()), Some(50));

    // An unmanaged key beside it writes as usual.
    landed(&source, settings.set_int("volume".into(), 10));
    assert_eq!(settings.get_int("volume".into()), Some(10));
}

#[test]
fn every_asset_writer_fails_for_a_managed_asset() {
    let dir = TempDir::new().unwrap();
    let (source, settings) = open(&dir, MANAGED);
    let file = dir.path().join("outside.yaml");
    std::fs::write(&file, "rows: []\n").unwrap();

    assert!(settings.set_asset("layout".into(), b"x".to_vec()).is_err());
    let path = file.display().to_string();
    assert!(settings.set_asset_file("layout".into(), path).is_err());
    assert!(settings.remove_asset("layout".into()).is_err());
    assert!(source.outcomes().is_empty());
    assert!(!dir.path().join("org.mechanix.demo/layout.yaml").exists());

    let ticket = settings
        .set_asset("theme".into(), b"body {}".to_vec())
        .unwrap();
    let outcomes = source.outcomes();
    let written = outcomes.iter().find(|o| o.ticket == ticket);
    let error = &written.expect("written").error;
    assert!(error.is_none(), "{error:?}");
    assert_eq!(
        settings.asset("theme".into()).as_deref(),
        Some(&b"body {}"[..])
    );
}

#[test]
fn a_user_value_for_a_managed_key_is_ignored_with_a_warning() {
    let dir = TempDir::new().unwrap();
    write_user(&dir, "brightness = 99\nvolume = 70\n");
    std::fs::write(dir.path().join("org.mechanix.demo/layout.yaml"), "user").unwrap();
    let (_source, settings) = open(&dir, MANAGED);

    assert_eq!(settings.get_int("brightness".into()), Some(50));
    assert_eq!(settings.get_int("volume".into()), Some(70));
    assert_eq!(settings.asset("layout".into()), None);
    let diagnostics = settings.diagnostics();
    let note = diagnostics
        .iter()
        .find(|d| d.key.as_deref() == Some("brightness"))
        .expect("a note for brightness");
    assert_eq!(note.severity, "warning");
    assert!(note.message.contains("managed"), "{}", note.message);
}

/// The open's problems arrive as a `Diagnosed` outcome after it.
#[test]
fn a_requested_open_reports_its_problems_once() {
    let dir = TempDir::new().unwrap();
    write_user(&dir, "[colors]\nbogus = 1\n");
    let source = MxSource::with_user_dir(dir.path().display().to_string());
    source.embed("org.mechanix.demo".into(), SCHEMA.into());
    source.request("org.mechanix.demo".into());

    let outcomes = source.outcomes();
    let kinds: Vec<_> = outcomes.iter().map(|o| format!("{:?}", o.kind)).collect();
    assert_eq!(kinds, ["Opened", "Diagnosed"]);
    let diagnostics = &outcomes[1].diagnostics;
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].key.as_deref(), Some("colors.bogus"));
}

const MANIFEST: &str = r#"
client = "org.mechanix.demo"

[[settings]]
namespace = "org.mechanix.demo"
access = "read"
"#;

const OTHER_MANIFEST: &str = r#"
client = "org.other.thing"

[[settings]]
namespace = "org.other.thing"
access = "read-write"
"#;

#[test]
fn a_manifest_gates_reads_and_writes() {
    let dir = TempDir::new().unwrap();
    let (_source, settings) = open(&dir, SCHEMA);

    // The manifest allows reads only, and a manifest for another namespace fails.
    settings
        .authorize(MANIFEST.into(), false)
        .expect("the manifest grants read");
    assert!(
        settings.authorize(MANIFEST.into(), true).is_err(),
        "a read-only manifest refuses a write"
    );
    assert!(
        settings.authorize(OTHER_MANIFEST.into(), false).is_err(),
        "an undeclared namespace is refused"
    );
}

#[test]
fn dart_loads_this_library_by_name() {
    let runtime = include_str!("../../../packages/mxconfig_dart/lib/src/runtime.dart");
    // The library's crate name is the first segment of its types' paths.
    let name = std::any::type_name::<MxconfigException>();
    let library = name.split("::").next().unwrap();
    let stem = format!("const _libStem = 'lib{library}.so';");
    assert!(runtime.contains(&stem), "{stem}");
}
