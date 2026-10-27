mod common;

use std::os::fd::OwnedFd;

// Edit a file from outside the source.
use common::put as edit;
use common::{install, schema};
use mxconfig::Schema;
use mxconfig::runtime::Drive;
use mxconfig::{Change, Outcome, Source};

/// An app namespace for the multi-namespace tests.
const ALPHA: &str = r#"
namespace = "org.mechanix.alpha"
version = 1

[keys.size]
type = "integer"
default = 1
label = "Size"
description = "Size."
"#;

/// A shared namespace, stored under mxconfig's directory.
const SHARED_NS: &str = "org.mechanix.desktop.panel";

const SHARED: &str = r#"
namespace = "org.mechanix.desktop.panel"
version = 1

[keys.size]
type = "integer"
default = 1
label = "Size"
description = "Size."
"#;

const BETA: &str = r#"
namespace = "org.mechanix.beta"
version = 1

[keys.count]
type = "integer"
default = 0
label = "Count"
description = "Count."
"#;

/// One read of the inotify fd.
fn read(fd: &OwnedFd) -> Vec<u8> {
    let mut buf = vec![0u8; 4096];
    let n = nix::unistd::read(fd, &mut buf).expect("the write queued an event");
    buf.truncate(n);
    buf
}

/// Feed one read and return the value changes.
fn feed(source: &Source, fd: &OwnedFd) -> Vec<Change> {
    source.feed_inotify(&read(fd));
    source
        .outcomes()
        .into_iter()
        .filter_map(|o| match o {
            Outcome::Changed(change) => Some(change),
            _ => None,
        })
        .collect()
}

/// Open `schema` through `source`.
fn open(source: &Source, schema: Schema) -> mxconfig::Settings {
    source.open_schema(schema).unwrap()
}

/// An outside edit produces a `Changed` once its read is fed in.
#[test]
fn an_outside_edit_arrives_through_the_inotify_fd() {
    let dir = tempfile::tempdir().unwrap();
    let source = common::in_dir(dir.path());
    let settings = open(&source, schema());
    let fd = source.inotify_fd().unwrap();

    let target = dir.path().join("org.mechanix.demo").join("config.toml");
    edit(&target, "[colors]\naccent = \"#ff0000\"\n");

    let changes = feed(&source, &fd);
    let change = changes
        .iter()
        .find(|c| c.key == "colors.accent")
        .expect("accent changed");
    assert_eq!(
        change.value,
        Some(mxconfig::Value::String("#ff0000".into()))
    );
    assert_eq!(change.revision, 1);
    assert_eq!(
        settings.get("colors.accent"),
        change.value,
        "the handle sees it too"
    );
}

/// A handle's own write is not read twice, but an outside edit right after it still is.
#[test]
fn an_outside_edit_after_an_own_write_is_read() {
    let dir = tempfile::tempdir().unwrap();
    let source = common::in_dir(dir.path());
    let settings = open(&source, schema());
    let fd = source.inotify_fd().unwrap();
    let mine = mxconfig::Value::String("#111111".into());
    settings
        .write(&[mxconfig::Edit::set("colors.accent", mine)])
        .unwrap();
    source.outcomes();
    assert!(
        feed(&source, &fd).is_empty(),
        "its own event changes nothing"
    );

    // The same length, written in place, so only the time and contents differ.
    let target = dir.path().join("org.mechanix.demo").join("config.toml");
    let text = std::fs::read_to_string(&target).unwrap();
    std::fs::write(&target, text.replace("#111111", "#222222")).unwrap();
    let changes = feed(&source, &fd);
    let theirs = Some(mxconfig::Value::String("#222222".into()));
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(settings.get("colors.accent"), theirs);
}

/// One fd covers every open namespace. Each change names its namespace.
#[test]
fn one_fd_covers_several_namespaces() {
    let dir = tempfile::tempdir().unwrap();
    let source = common::in_dir(dir.path());
    let _alpha = open(&source, Schema::parse(ALPHA).unwrap());
    let _beta = open(&source, Schema::parse(BETA).unwrap());
    let fd = source.inotify_fd().unwrap();

    let alpha = dir.path().join("org.mechanix.alpha").join("config.toml");
    edit(&alpha, "size = 42\n");
    let changes = feed(&source, &fd);
    assert!(
        changes.iter().all(|c| c.namespace == "org.mechanix.alpha"),
        "{changes:?}"
    );
    assert_eq!(changes[0].value, Some(mxconfig::Value::Integer(42)));

    let beta = dir.path().join("org.mechanix.beta").join("config.toml");
    edit(&beta, "count = 7\n");
    let changes = feed(&source, &fd);
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].key, "count");
}

/// A shared namespace's directory created later is watched.
#[test]
fn a_directory_that_appears_later_is_watched() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("config");
    let source = common::in_dir(&dir);
    let _settings = open(&source, Schema::parse(SHARED).unwrap());
    let fd = source.inotify_fd().unwrap();

    let path = dir.join("mxconfig").join(SHARED_NS).join("conf.toml");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    assert!(
        feed(&source, &fd).is_empty(),
        "a new directory changes no value"
    );
    edit(&path, "size = 9\n");
    let changes = feed(&source, &fd);
    assert_eq!(
        changes.iter().find(|c| c.key == "size").unwrap().value,
        Some(mxconfig::Value::Integer(9))
    );
}

/// A directory that can't be watched is reported as a diagnostic.
#[test]
fn a_directory_that_cannot_be_watched_shows_in_the_diagnostics() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let locked = dir.path().join("org.mechanix.alpha");
    std::fs::create_dir(&locked).unwrap();
    let mode = |bits| std::fs::Permissions::from_mode(bits);
    std::fs::set_permissions(&locked, mode(0o000)).unwrap();
    if std::fs::read_dir(&locked).is_ok() {
        return std::fs::set_permissions(&locked, mode(0o700)).unwrap(); // root
    }

    let source = common::in_dir(dir.path());
    let settings = open(&source, Schema::parse(ALPHA).unwrap());
    let _fd = source.inotify_fd().unwrap();
    let diagnostics = settings.diagnostics();
    std::fs::set_permissions(&locked, mode(0o700)).unwrap();
    assert!(
        diagnostics
            .iter()
            .any(|d| d.message().contains("cannot watch")),
        "{diagnostics:?}"
    );
}

/// The value changes in `source`'s outcomes.
fn changes(source: &Source) -> Vec<Change> {
    let outcomes = source.outcomes().into_iter();
    let changed = outcomes.filter_map(|o| match o {
        Outcome::Changed(change) => Some(change),
        _ => None,
    });
    changed.collect()
}

/// An app's directory is created at open and watched, but not its base.
#[test]
fn an_app_watches_its_own_directory_from_the_start() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("config");
    let source = common::in_dir(&dir);
    let _alpha = open(&source, Schema::parse(ALPHA).unwrap());
    let _fd = source.inotify_fd().unwrap();
    assert_eq!(
        source.take_watch_dirs().unwrap(),
        vec![dir.join("org.mechanix.alpha")]
    );
}

/// A shared namespace's missing directory is watched through its parent.
#[test]
fn a_stand_in_watch_ends_once_the_directory_exists() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("config");
    let source = common::in_dir(&dir);
    let _shared = open(&source, Schema::parse(SHARED).unwrap());
    let fd = source.inotify_fd().unwrap();
    assert_eq!(
        source.take_watch_dirs().unwrap(),
        vec![dir.join("mxconfig")]
    );

    let ns_dir = dir.join("mxconfig").join(SHARED_NS);
    std::fs::create_dir_all(&ns_dir).unwrap();
    feed(&source, &fd);
    assert_eq!(
        source.take_watch_dirs().unwrap(),
        vec![ns_dir],
        "mxconfig's is let go"
    );
}

/// Nothing above a layer root is watched, nor a missing system root.
#[test]
fn nothing_outside_the_layer_roots_is_watched() {
    let tmp = tempfile::tempdir().unwrap();
    let roots = mxconfig::host::Roots::under(tmp.path());
    install(&roots, "org.mechanix.alpha", ALPHA);
    let source = Source::with_roots(roots.clone());
    let _alpha = common::open(&source, "org.mechanix.alpha").unwrap();
    let _fd = source.inotify_fd().unwrap();
    let user = roots.user_dir.clone().unwrap();
    let home = user.join("org.mechanix.alpha");
    let missing = "vendor and admin do not exist";
    assert_eq!(source.take_watch_dirs().unwrap(), vec![home], "{missing}");
}

/// The vendor layer is read but not watched.
#[test]
fn the_vendor_layer_is_not_watched() {
    let tmp = tempfile::tempdir().unwrap();
    let roots = mxconfig::host::Roots::under(tmp.path());
    install(&roots, "org.mechanix.alpha", ALPHA);
    let vendor = roots.vendor_dir.clone().unwrap().join("org.mechanix.alpha");
    edit(&vendor.join("config.toml"), "size = 3\n");

    let source = Source::with_roots(roots.clone());
    let alpha = common::open(&source, "org.mechanix.alpha").unwrap();
    assert_eq!(
        alpha.get("size"),
        Some(mxconfig::Value::Integer(3)),
        "it is read"
    );
    let _fd = source.inotify_fd().unwrap();
    let dirs = source.take_watch_dirs().unwrap();
    assert!(!dirs.iter().any(|d| d.starts_with(&vendor)), "{dirs:?}");
}

/// An event re-reads only the namespaces whose files it affects.
#[test]
fn an_event_rereads_only_the_namespaces_it_concerns() {
    let dir = tempfile::tempdir().unwrap();
    let source = common::in_dir(dir.path());
    let _alpha = open(&source, Schema::parse(ALPHA).unwrap());
    let _beta = open(&source, Schema::parse(BETA).unwrap());
    let alpha = dir.path().join("org.mechanix.alpha").join("config.toml");
    let beta = dir.path().join("org.mechanix.beta").join("config.toml");
    edit(&alpha, "size = 42\n");
    edit(&beta, "count = 7\n");

    source.touched(&dir.path().join("unrelated.txt"));
    assert!(changes(&source).is_empty(), "nothing open lives there");
    source.touched(&alpha);
    let seen = changes(&source);
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].namespace, "org.mechanix.alpha");
    source.touched(&beta);
    assert_eq!(changes(&source)[0].namespace, "org.mechanix.beta");
}

/// Only files a namespace reads trigger a re-read.
#[test]
fn only_files_the_namespace_reads_count() {
    let dir = tempfile::tempdir().unwrap();
    let source = common::in_dir(dir.path());
    let _alpha = open(&source, Schema::parse(ALPHA).unwrap());
    let home = dir.path().join("org.mechanix.alpha");
    edit(&home.join("config.toml"), "size = 5\n");

    let ignored = [
        ".tmpAbC123",
        "config.toml.lastgood",
        "config.toml.state",
        "notes.txt",
    ];
    for name in ignored {
        source.touched(&home.join(name));
        assert!(changes(&source).is_empty(), "{name} is not read");
    }
    source.touched(&home.join("config.toml"));
    assert_eq!(changes(&source).len(), 1);

    let drop_in = home.join("config.d").join("10-size.toml");
    edit(&drop_in, "size = 6\n");
    source.touched(&home.join("config.d").join("README"));
    assert!(changes(&source).is_empty(), "only *.toml drop-ins are read");
    source.touched(&drop_in);
    assert_eq!(changes(&source).len(), 1);
}

/// A removed and recreated namespace directory is watched again.
#[test]
fn a_directory_made_again_is_watched_again() {
    let dir = tempfile::tempdir().unwrap();
    let source = common::in_dir(dir.path());
    let _alpha = open(&source, Schema::parse(ALPHA).unwrap());
    let ns_dir = dir.path().join("org.mechanix.alpha");
    edit(&ns_dir.join("config.toml"), "size = 2\n");
    let fd = source.inotify_fd().unwrap();

    // The user home is recreated immediately and watched.
    std::fs::remove_dir_all(&ns_dir).unwrap();
    feed(&source, &fd);
    assert!(ns_dir.is_dir());
    edit(&ns_dir.join("config.toml"), "size = 3\n");
    let seen = feed(&source, &fd);
    assert_eq!(seen[0].value, Some(mxconfig::Value::Integer(3)), "{seen:?}");
}

/// A new file is read when its writer closes it, not before.
#[test]
fn a_new_file_is_read_once_its_writer_closes_it() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let source = common::in_dir(dir.path());
    let settings = open(&source, schema());
    let fd = source.inotify_fd().unwrap();

    let target = dir.path().join("org.mechanix.demo").join("config.toml");
    let mut file = std::fs::File::create(&target).unwrap();
    // A partial file can parse, so reading it now would apply a value early.
    file.write_all(b"[colors]\naccent = \"#ff0000\"\n").unwrap();
    assert!(feed(&source, &fd).is_empty(), "nothing before the close");
    drop(file);
    let changes = feed(&source, &fd);
    let red = Some(mxconfig::Value::String("#ff0000".into()));
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(changes[0].value, red);
    assert_eq!(settings.get("colors.accent"), red);
}

/// A symlinked file is read immediately.
#[test]
fn a_file_linked_into_place_is_read_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let source = common::in_dir(dir.path());
    let settings = open(&source, schema());
    let real = dir.path().join("dotfiles.toml");
    std::fs::write(&real, "[colors]\naccent = \"#00ff00\"\n").unwrap();
    let fd = source.inotify_fd().unwrap();

    let target = dir.path().join("org.mechanix.demo").join("config.toml");
    std::os::unix::fs::symlink(&real, &target).unwrap();
    let changes = feed(&source, &fd);
    let green = Some(mxconfig::Value::String("#00ff00".into()));
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(settings.get("colors.accent"), green);
}

/// Edits to a linked file's target are followed, as dotfile managers link them.
#[test]
fn a_linked_file_follows_edits_to_its_target() {
    let dir = tempfile::tempdir().unwrap();
    let dotfiles = tempfile::tempdir().unwrap();
    let source = common::in_dir(dir.path());
    let settings = open(&source, schema());
    let real = dotfiles.path().join("demo.toml");
    std::fs::write(&real, "[colors]\naccent = \"#00ff00\"\n").unwrap();
    let target = dir.path().join("org.mechanix.demo").join("config.toml");
    std::os::unix::fs::symlink(&real, &target).unwrap();
    let fd = source.inotify_fd().unwrap();
    feed(&source, &fd);

    std::fs::write(&real, "[colors]\naccent = \"#0000ff\"\n").unwrap();
    feed(&source, &fd);
    let blue = Some(mxconfig::Value::String("#0000ff".into()));
    assert_eq!(settings.get("colors.accent"), blue);

    // Pointed elsewhere, the link follows its new target instead.
    let other = dotfiles.path().join("other").join("demo.toml");
    edit(&other, "[colors]\naccent = \"#111111\"\n");
    std::fs::remove_file(&target).unwrap();
    std::os::unix::fs::symlink(&other, &target).unwrap();
    feed(&source, &fd);
    std::fs::write(&other, "[colors]\naccent = \"#222222\"\n").unwrap();
    feed(&source, &fd);
    let grey = Some(mxconfig::Value::String("#222222".into()));
    assert_eq!(settings.get("colors.accent"), grey);
}

/// `ALPHA` at version 2.
fn alpha_v2() -> String {
    ALPHA.replace("version = 1", "version = 2")
}

/// Install `schema` in a fake Flatpak deploy `commit` under `branch`.
fn deploy(branch: &std::path::Path, commit: &str, schema: &str) {
    let schemas = branch.join(commit).join("files/share/mxconfig/schemas");
    std::fs::create_dir_all(&schemas).unwrap();
    std::fs::write(schemas.join("org.mechanix.alpha.toml"), schema).unwrap();
}

/// A serving source watches no schema directory, as an open namespace keeps its schema.
#[test]
fn a_serving_source_watches_no_schema_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let roots = mxconfig::host::Roots {
        serving: Some(mxconfig::Bus::Session),
        ..mxconfig::host::Roots::under(tmp.path())
    };
    install(&roots, "org.mechanix.alpha", ALPHA);
    let source = Source::with_roots(roots.clone());
    let _alpha = common::open(&source, "org.mechanix.alpha").unwrap();
    let dirs = source.take_watch_dirs().unwrap();
    let schemas = &roots.schema_dirs[0];
    for dir in &dirs {
        assert!(
            !dir.starts_with(schemas) && !schemas.starts_with(dir),
            "{dir:?} in {dirs:?}"
        );
    }
}

/// A Flatpak update swaps the deploy, and a serving source's next open reads it.
#[test]
fn a_serving_source_reads_a_flatpak_update_on_the_next_open() {
    let tmp = tempfile::tempdir().unwrap();
    let branch = tmp.path().join("app/org.mechanix.alpha/x86_64/stable");
    deploy(&branch, "aaa", ALPHA);
    std::os::unix::fs::symlink("aaa", branch.join("active")).unwrap();
    let mut roots =
        mxconfig::host::Roots::flatpak(&tmp.path().join("instance"), &branch.join("aaa/files"));
    roots.serving = Some(mxconfig::Bus::Session);
    let source = Source::with_roots(roots);
    let alpha = common::open(&source, "org.mechanix.alpha").unwrap();

    // Flatpak deploys the new commit, moves `active`, then removes the old one.
    deploy(&branch, "bbb", &alpha_v2());
    std::os::unix::fs::symlink("bbb", branch.join("next")).unwrap();
    std::fs::rename(branch.join("next"), branch.join("active")).unwrap();
    std::fs::remove_dir_all(branch.join("aaa")).unwrap();
    source.rescan();
    let schema = |o: &Outcome| matches!(o, Outcome::Detached { .. });
    assert!(!source.outcomes().iter().any(schema));
    assert_eq!(alpha.describe().version(), 1, "held while open");

    drop(alpha);
    let alpha = common::open(&source, "org.mechanix.alpha").unwrap();
    assert_eq!(alpha.describe().version(), 2);
}

/// Roots over `tmp` with `ALPHA` installed and no admin home for it.
fn missing_admin(tmp: &std::path::Path, serving: Option<mxconfig::Bus>) -> mxconfig::host::Roots {
    let roots = mxconfig::host::Roots {
        serving,
        ..mxconfig::host::Roots::under(tmp)
    };
    std::fs::create_dir_all(roots.admin_dir.as_ref().unwrap()).unwrap();
    install(&roots, "org.mechanix.alpha", ALPHA);
    roots
}

/// A daemon sees an admin home created after it opened the namespace.
#[test]
fn a_daemon_sees_an_admin_home_appear() {
    let tmp = tempfile::tempdir().unwrap();
    let roots = missing_admin(tmp.path(), Some(mxconfig::Bus::Session));
    let admin = roots.admin_dir.clone().unwrap();
    let source = Source::with_roots(roots);
    let fd = source.inotify_fd().unwrap();
    let alpha = common::open(&source, "org.mechanix.alpha").unwrap();
    let dirs = source.take_watch_dirs().unwrap();
    assert!(
        dirs.contains(&admin),
        "the base, while the home is missing: {dirs:?}"
    );

    // An admin makes the home, then writes the file in it.
    std::fs::create_dir(admin.join("org.mechanix.alpha")).unwrap();
    source.feed_inotify(&read(&fd));
    let dirs = source.take_watch_dirs().unwrap();
    assert!(!dirs.contains(&admin), "the base is let go: {dirs:?}");
    let tmp_file = admin.join("org.mechanix.alpha/.config.tmp");
    std::fs::write(&tmp_file, "size = 4\n").unwrap();
    std::fs::rename(&tmp_file, admin.join("org.mechanix.alpha/config.toml")).unwrap();
    source.feed_inotify(&read(&fd));
    assert_eq!(alpha.get("size"), Some(mxconfig::Value::Integer(4)));
}

/// An app's source never watches a layer's base. It sees a new home on restart.
#[test]
fn an_app_never_watches_a_layer_base() {
    let tmp = tempfile::tempdir().unwrap();
    let roots = missing_admin(tmp.path(), None);
    let admin = roots.admin_dir.clone().unwrap();
    let source = Source::with_roots(roots);
    let _alpha = common::open(&source, "org.mechanix.alpha").unwrap();
    let dirs = source.take_watch_dirs().unwrap();
    assert!(!dirs.contains(&admin), "{dirs:?}");
}
