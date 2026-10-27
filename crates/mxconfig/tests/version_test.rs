//! One schema version per namespace at a time, between processes reading files.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{Through, at, edge, key};
use mxconfig::host::Roots;
use mxconfig::runtime::Drive;
use mxconfig::{Detach, Edit, Error, ErrorKind, Outcome, Schema, Settings, Source, Value};

const NS: &str = "org.example.app";

/// v1 has `width`. v2 renames it to `size`.
fn v1(namespace: &str) -> String {
    at(namespace, 1, &key("width", "integer"))
}

fn v2(namespace: &str) -> String {
    let rename = edge(1, 2, &[r#"{ op = "rename", from = "width", to = "size" }"#]);
    at(namespace, 2, &(key("size", "integer") + &rename))
}

/// Roots in a new temporary directory.
fn tree() -> (tempfile::TempDir, Roots) {
    let tmp = tempfile::tempdir().unwrap();
    let roots = Roots::under(tmp.path());
    fs::create_dir_all(&roots.schema_dirs[0]).unwrap();
    (tmp, roots)
}

/// Open `text` over `roots` on a source of its own, as another process would.
fn open(roots: &Roots, text: &str) -> Result<Settings, Error> {
    Source::with_roots(roots.clone()).open_schema(Schema::parse(text).unwrap())
}

/// The user file of [`NS`].
fn user_file(roots: &Roots) -> PathBuf {
    roots
        .user_dir
        .as_ref()
        .unwrap()
        .join(NS)
        .join("config.toml")
}

/// The version recorded beside `file`.
fn recorded(file: &Path) -> i64 {
    common::recorded(file).expect("a state file").0
}

/// Whether `error` is a version conflict naming both versions.
fn conflict(error: &Error, held: u32, own: u32) -> bool {
    let text = error.to_string();
    error.kind() == ErrorKind::VersionConflict
        && text.contains(&format!("v{held}"))
        && text.contains(&format!("v{own}"))
}

/// Opens of one version share the namespace.
#[test]
fn the_same_version_opens_beside_itself() {
    let (_tmp, roots) = tree();
    let first = open(&roots, &v1(NS)).unwrap();
    let second = open(&roots, &v1(NS)).unwrap();
    first.set("width", Value::Integer(5)).unwrap();
    assert_eq!(second.reload().len(), 1);
    assert_eq!(second.get("width"), Some(Value::Integer(5)));
}

/// A newer version doesn't open while an older one holds the file, and nothing moves.
#[test]
fn a_newer_version_is_refused_while_an_older_one_holds() {
    let (_tmp, roots) = tree();
    let older = open(&roots, &v1(NS)).unwrap();
    older.set("width", Value::Integer(5)).unwrap();
    let before = fs::read_to_string(user_file(&roots)).unwrap();

    let refused = open(&roots, &v2(NS)).unwrap_err();
    assert!(conflict(&refused, 1, 2), "{refused}");
    assert_eq!(fs::read_to_string(user_file(&roots)).unwrap(), before);
    assert_eq!(recorded(&user_file(&roots)), 1);
    // The older one carries on as if nothing happened.
    older.set("width", Value::Integer(6)).unwrap();
    assert_eq!(older.get("width"), Some(Value::Integer(6)));
}

/// An older version doesn't open while a newer one holds the file.
#[test]
fn an_older_version_is_refused_while_a_newer_one_holds() {
    let (_tmp, roots) = tree();
    let newer = open(&roots, &v2(NS)).unwrap();
    newer.set("size", Value::Integer(3)).unwrap();
    let refused = open(&roots, &v1(NS)).unwrap_err();
    assert!(conflict(&refused, 2, 1), "{refused}");
    assert_eq!(recorded(&user_file(&roots)), 2);
}

/// The version is let go with the last handle, and the next version then migrates.
#[test]
fn the_last_handle_going_lets_the_next_version_in() {
    let (_tmp, roots) = tree();
    let older = open(&roots, &v1(NS)).unwrap();
    older.set("width", Value::Integer(5)).unwrap();
    let other = older.clone();
    drop(older);
    let refused = open(&roots, &v2(NS)).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::VersionConflict);
    assert_eq!(refused.other_version(), Some(1), "v1 holds it");

    drop(other);
    let newer = open(&roots, &v2(NS)).unwrap();
    assert_eq!(newer.get("size"), Some(Value::Integer(5)));
    assert_eq!(recorded(&user_file(&roots)), 2);
}

/// A typed client reads with the schema it carries, not a newer installed one.
#[test]
fn a_client_reads_with_the_schema_it_carries() {
    let (_tmp, roots) = tree();
    common::install(&roots, NS, &v2(NS));
    let client = Source::with_roots(roots.clone());
    client.embed(NS, &v1(NS));
    let settings = common::open(&client, NS).unwrap();
    assert_eq!(settings.describe().version(), 1);
    settings.set("width", Value::Integer(4)).unwrap();

    // A tool reading the installed v2 is kept out while the client holds v1.
    let tool = common::open(&Source::with_roots(roots.clone()), NS);
    assert!(conflict(&tool.unwrap_err(), 1, 2));
    drop(settings);
    let tool = common::open(&Source::with_roots(roots), NS).unwrap();
    assert_eq!(tool.get("size"), Some(Value::Integer(4)));
}

/// A file moved to another version behind the locks detaches the handle.
#[test]
fn a_file_moved_behind_the_locks_detaches_the_handle() {
    let (_tmp, roots) = tree();
    let source = Source::with_roots(roots.clone());
    let settings = source.open_schema(Schema::parse(&v1(NS)).unwrap()).unwrap();
    settings.set("width", Value::Integer(5)).unwrap();
    source.outcomes();

    // Something that ignores the locks migrates the file to v2.
    let file = user_file(&roots);
    fs::write(&file, "size = 9\n").unwrap();
    fs::write(file.with_file_name("config.toml.state"), "version = 2\n").unwrap();
    source.rescan();
    let outcomes = source.outcomes();
    let detached = |o: &Outcome| matches!(o, Outcome::Detached { namespace, cause: Detach::Moved } if namespace == NS);
    assert!(outcomes.iter().any(detached), "{outcomes:?}");
    assert!(!outcomes.iter().any(|o| matches!(o, Outcome::Changed(_))));

    // It keeps the values it had, refuses writes, and says why.
    assert_eq!(settings.get("width"), Some(Value::Integer(5)));
    assert_eq!(
        settings.set("width", Value::Integer(6)).unwrap_err().kind(),
        ErrorKind::Detached
    );
    assert_eq!(settings.prune().unwrap_err().kind(), ErrorKind::Detached);
    let said = |d: &mxconfig::Diagnostic| d.message().contains("detached");
    assert!(settings.diagnostics().iter().any(said));
    assert_eq!(fs::read_to_string(&file).unwrap(), "size = 9\n");

    // It is told once.
    source.rescan();
    assert!(source.outcomes().is_empty());
}

/// Writes from several processes at once all land, as each holds the write lock.
#[test]
fn concurrent_writers_lose_nothing() {
    let (_tmp, roots) = tree();
    let keys: String = (0..40).map(|i| key(&format!("k{i}"), "integer")).collect();
    let schema = at(NS, 1, &keys);
    let writers: Vec<_> = (0..4)
        .map(|w| {
            let (roots, schema) = (roots.clone(), schema.clone());
            std::thread::spawn(move || {
                let settings = open(&roots, &schema).unwrap();
                for i in (w..40).step_by(4) {
                    let edit = Edit::set(format!("k{i}"), i64::from(i));
                    settings.apply(&[edit]).unwrap();
                }
            })
        })
        .collect();
    for writer in writers {
        writer.join().unwrap();
    }
    let settings = open(&roots, &schema).unwrap();
    for i in 0..40 {
        let key = format!("k{i}");
        assert_eq!(settings.get(&key), Some(Value::Integer(i)), "{key}");
    }
}

/// The lock sits beside the namespace's home, which is made private.
#[test]
fn the_lock_sits_beside_the_home() {
    use std::os::unix::fs::PermissionsExt;
    let (_tmp, roots) = tree();
    let _settings = open(&roots, &v1(NS)).unwrap();
    let user = roots.user_dir.as_ref().unwrap();
    assert!(user.join(format!("{NS}.lock")).is_file());
    let mode = fs::metadata(user).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700);
}

/// A home that can't be locked still opens, with a warning.
#[test]
fn an_unlockable_home_opens_with_a_warning() {
    use std::os::unix::fs::PermissionsExt;
    let (_tmp, roots) = tree();
    let user = roots.user_dir.clone().unwrap();
    fs::create_dir_all(&user).unwrap();
    fs::set_permissions(&user, fs::Permissions::from_mode(0o500)).unwrap();
    if fs::write(user.join("probe"), "").is_ok() {
        return fs::set_permissions(&user, fs::Permissions::from_mode(0o700)).unwrap(); // root
    }
    let settings = open(&roots, &v1(NS));
    fs::set_permissions(&user, fs::Permissions::from_mode(0o700)).unwrap();
    let settings = settings.unwrap();
    let warned = |d: &mxconfig::Diagnostic| d.message().contains("cannot lock");
    assert!(
        settings.diagnostics().iter().any(warned),
        "{:?}",
        settings.diagnostics()
    );
}

/// A file that needs migrating fails to open when its lock can't be held.
#[test]
fn an_unlockable_home_refuses_to_migrate() {
    use std::os::unix::fs::PermissionsExt;
    let (_tmp, roots) = tree();
    open(&roots, &v1(NS))
        .unwrap()
        .set("width", Value::Integer(5))
        .unwrap();
    let file = user_file(&roots);
    let user = roots.user_dir.clone().unwrap();
    fs::remove_file(user.join(format!("{NS}.lock"))).unwrap();
    fs::set_permissions(&user, fs::Permissions::from_mode(0o500)).unwrap();
    if fs::write(user.join("probe"), "").is_ok() {
        return fs::set_permissions(&user, fs::Permissions::from_mode(0o700)).unwrap(); // root
    }
    let opened = open(&roots, &v2(NS));
    fs::set_permissions(&user, fs::Permissions::from_mode(0o700)).unwrap();
    let e = opened.unwrap_err();
    assert!(e.to_string().contains("can't be held"), "{e}");
    assert_eq!(recorded(&file), 1, "the file is untouched");
}

/// A version whose names read the same opens while another holds, but only reads.
#[test]
fn another_version_reading_untouched_names_only_reads() {
    let (_tmp, roots) = tree();
    let both = |version: u32| {
        let text = if version == 1 { v1(NS) } else { v2(NS) };
        text + &key("depth", "integer")
    };
    let older = open(&roots, &both(1)).unwrap();
    older.set("depth", Value::Integer(4)).unwrap();

    // v2 renamed `width`, so only a v2 reading `depth` alone may open.
    assert!(open(&roots, &both(2)).is_err());
    let newer = Schema::parse(&both(2))
        .unwrap()
        .trimmed(&["depth"])
        .unwrap();
    let reader = Source::with_roots(roots.clone())
        .open_schema(newer)
        .unwrap();
    assert_eq!(reader.other_version(), Some(1));
    assert_eq!(reader.get("depth"), Some(Value::Integer(4)));
    let refused = reader.write(&[Edit::set("depth", 5)]).unwrap_err();
    assert!(conflict(&refused, 1, 2), "{refused}");
    assert_eq!(older.get("depth"), Some(Value::Integer(4)));
}
