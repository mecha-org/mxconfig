//! Tests for the generated client, used as an app would.

use std::fs;

use mxconfig::host::Roots;
use mxconfig::{ErrorKind, Event, FromSettings, Outcome, Schema, Source, Value};
use mxconfig_example::{ColorsMode, Demo, DemoChange};
use tempfile::TempDir;

/// The v1 schema: has `colors.radius` but no `colors.mode`.
const V1: &str = r#"
namespace = "org.mechanix.demo"
version = 1

[groups.colors]
label = "Colors"

[keys.colors.radius]
type = "integer"
default = 8
label = "Corner radius"
description = "Corner radius in pixels."
"#;

/// A source whose only layer is the user directory `dir`, for tests.
fn in_dir(dir: &TempDir) -> Source {
    Source::with_roots(Roots {
        user_dir: Some(dir.path().into()),
        ..Roots::default()
    })
}

/// Open the demo namespace as an app does.
fn opened(dir: &TempDir) -> (Source, Demo) {
    let source = in_dir(dir);
    let demo = request::<Demo>(&source).unwrap();
    (source, demo)
}

/// Request client `T` from files, which answer at once.
fn request<T: FromSettings + mxconfig::Namespace>(source: &Source) -> Result<T, mxconfig::Error> {
    let ticket = source.request::<T>();
    let outcomes = source.outcomes();
    outcomes
        .iter()
        .find_map(|o| ticket.opened(o))
        .expect("answered at once")
}

/// An app handles its namespace's outcomes as typed events, with no tickets.
#[test]
fn an_app_handles_typed_events() {
    let dir = TempDir::new().unwrap();
    let source = in_dir(&dir);
    source.request::<Demo>();

    let mut events = Vec::new();
    let mut demo = None;
    for outcome in source.outcomes() {
        match outcome.event::<Demo>() {
            Some(Event::Opened(opened)) => {
                opened.colors().set_accent("#ff0000").unwrap();
                demo = Some(opened);
            }
            Some(event) => events.push(event),
            None => {}
        }
    }
    assert!(demo.is_some(), "files answer at once");
    for outcome in source.outcomes() {
        events.extend(outcome.event::<Demo>());
    }
    let changed = DemoChange::ColorsAccent("#ff0000".into());
    assert!(matches!(&*events, [Event::Changed(c), Event::Written] if *c == changed));

    // Another namespace's outcomes are not this client's events.
    let other = Outcome::Detached {
        namespace: "org.mechanix.other".into(),
        cause: mxconfig::Detach::Moved,
    };
    assert!(other.event::<Demo>().is_none());
}

#[test]
fn an_app_reads_its_defaults() {
    let dir = TempDir::new().unwrap();
    let (_source, demo) = opened(&dir);

    assert_eq!(demo.colors().accent(), "#7aa2f7");
    assert_eq!(demo.colors().corner_radius(), 8);
    assert_eq!(demo.colors().mode(), ColorsMode::Dark);
    assert_eq!(demo.colors().scale(), 1.0);
    assert!(!demo.verbose());
    assert_eq!(demo.nickname(), None, "a key with no default starts unset");
    assert_eq!(demo.settings().revision(), 0);
    assert_eq!(demo.settings().describe().keys().len(), 7);
}

#[test]
fn an_app_writes_and_observes_change() {
    let dir = TempDir::new().unwrap();
    let (source, demo) = opened(&dir);

    // `Written` reports success. The change arrives as a typed `Changed`.
    let write = demo.colors().set_accent("#ff0000").unwrap();
    let outcomes = source.outcomes();
    outcomes
        .iter()
        .find_map(|o| write.written(o))
        .unwrap()
        .unwrap();
    let typed: Vec<DemoChange> = outcomes
        .iter()
        .filter_map(Outcome::change::<Demo>)
        .collect();
    assert_eq!(typed, vec![DemoChange::ColorsAccent("#ff0000".into())]);
    assert_eq!(demo.colors().accent(), "#ff0000");
    assert_eq!(demo.settings().revision(), 1);

    demo.colors().set_mode(ColorsMode::Light).unwrap();
    assert_eq!(demo.colors().mode(), ColorsMode::Light);
    assert_eq!(ColorsMode::Light.as_str(), "light");
    assert_eq!(ColorsMode::parse("dark"), Some(ColorsMode::Dark));
    assert_eq!(ColorsMode::parse("sepia"), None);

    demo.set_nickname("aux").unwrap();
    assert_eq!(demo.nickname(), Some("aux".to_string()));
    demo.clear_nickname().unwrap();
    assert_eq!(demo.nickname(), None);

    let refused = demo.colors().set_scale(9.0);
    assert!(refused.is_err(), "out of range");
    demo.set_verbose(true).unwrap();
    assert!(demo.verbose());

    // Datetimes are read and written as datetimes.
    let when: mxconfig::Datetime = "1979-05-27T07:32:00Z".parse().unwrap();
    demo.set_last_seen(when).unwrap();
    assert_eq!(demo.last_seen(), Some(when));

    // The typed client and the generic handle agree.
    assert_eq!(
        demo.settings().get("colors.accent"),
        Some(mxconfig::Value::String("#ff0000".into()))
    );
}

#[test]
fn an_old_settings_file_migrates_on_open() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("org.mechanix.demo").join("config.toml");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "[colors]\nradius = 12\n").unwrap();
    fs::write(path.with_file_name("config.toml.state"), "version = 1\n").unwrap();

    // Opening the v1 file migrates it to the schema's version.
    let (_source, demo) = opened(&dir);
    assert_eq!(demo.settings().describe().version(), 2);
    assert_eq!(
        demo.colors().corner_radius(),
        12,
        "the renamed key kept its value"
    );
    assert_eq!(
        demo.colors().mode(),
        ColorsMode::Dark,
        "the added key took the default"
    );

    demo.colors().set_mode(ColorsMode::Light).unwrap();
    // A v2 handle holds the file, so v1 opens only once it is gone.
    let refused = in_dir(&dir).open_schema(Schema::parse(V1).unwrap());
    assert_eq!(refused.unwrap_err().kind(), ErrorKind::VersionConflict);
    drop(demo);

    // A downgrade undoes the rename. The newer key stays in the file, unknown to v1.
    let v1 = in_dir(&dir)
        .open_schema(Schema::parse(V1).unwrap())
        .unwrap();
    assert_eq!(v1.describe().version(), 1);
    assert_eq!(v1.get("colors.radius"), Some(Value::Integer(12)));
    assert_eq!(v1.get("colors.mode"), None, "unknown to v1");
    drop(v1);

    // Upgrading again finds the user's value where it was.
    let (_source, demo) = opened(&dir);
    assert_eq!(demo.colors().mode(), ColorsMode::Light);
}

/// A client of only the keys it reads.
mod trimmed {
    mxconfig::client!("schema/org.mechanix.demo.toml", keys = ["colors.accent"]);
}

#[test]
fn a_trimmed_client_carries_only_its_keys() {
    use mxconfig::Namespace;
    let carried = Schema::parse(trimmed::Demo::SCHEMA).unwrap();
    let paths: Vec<&str> = carried.keys().iter().map(|k| k.path.as_str()).collect();
    assert_eq!(paths, ["colors.accent"]);

    let dir = TempDir::new().unwrap();
    let demo: trimmed::Demo = request(&in_dir(&dir)).unwrap();
    assert_eq!(demo.colors().accent(), "#7aa2f7");
    drop(demo);

    // While an older build holds the file, it reads, since v2 renamed only `colors.radius`.
    let _older = in_dir(&dir)
        .open_schema(Schema::parse(V1).unwrap())
        .unwrap();
    let demo: trimmed::Demo = request(&in_dir(&dir)).unwrap();
    assert_eq!(demo.settings().other_version(), Some(1));
    assert_eq!(demo.colors().accent(), "#7aa2f7");
    let refused = demo.colors().set_accent("#ff0000").unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::VersionConflict, "{refused}");
}
