//! Tests for the schema search path, layers and drop-ins.

mod common;

use common::Through;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use mxconfig::LayerKind;
use mxconfig::Settings;
use mxconfig::Value;
use mxconfig::host::{Home, Roots};
use mxconfig::{Origin, Schema, Source};

const NS: &str = "org.mechanix.demo";

const SCHEMA: &str = r#"
namespace = "org.mechanix.demo"
version = 1

[keys.a]
type = "integer"
default = 0
label = "A"
description = "d"

[keys.b]
type = "integer"
default = 0
label = "B"
description = "d"
"#;

/// Write `text` to `<root>/<NS>/<rel>`.
fn put(root: &Path, rel: &str, text: &str) -> PathBuf {
    common::put(&root.join(NS).join(rel), text)
}

fn open(roots: &Roots) -> Settings {
    common::install(roots, NS, SCHEMA);
    common::open(&Source::with_roots(roots.clone()), NS).unwrap()
}

#[test]
fn the_first_schema_on_the_search_path_wins() {
    let tmp = tempfile::tempdir().unwrap();
    let mut roots = Roots::under(tmp.path());
    let system = roots.schema_dirs[0].clone();
    let local = tmp.path().join("local");
    roots.schema_dirs.insert(0, local.clone());
    for (dir, schema) in [
        (&system, SCHEMA.to_string()),
        (&local, SCHEMA.replace("default = 0", "default = 7")),
    ] {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join(format!("{NS}.toml")), schema).unwrap();
    }

    let settings = common::open(&Source::with_roots(roots.clone()), NS).unwrap();
    assert_eq!(settings.get("a"), Some(Value::Integer(7)));
    assert_eq!(roots.schema_paths(NS).count(), 2);
    assert_eq!(roots.schema_files(), vec![local.join(format!("{NS}.toml"))]);
}

#[test]
fn system_drop_ins_merge_by_name_and_can_be_masked() {
    let tmp = tempfile::tempdir().unwrap();
    let roots = Roots::under(tmp.path());
    put(
        roots.vendor_dir.as_ref().unwrap(),
        "config.d/10-a.toml",
        "a = 1\n",
    );
    put(
        roots.vendor_dir.as_ref().unwrap(),
        "config.d/20-b.toml",
        "b = 2\n",
    );
    // An admin drop-in replaces the vendor drop-in with the same name.
    put(
        roots.admin_dir.as_ref().unwrap(),
        "config.d/10-a.toml",
        "a = 3\n",
    );
    // An empty admin drop-in masks the vendor drop-in.
    put(roots.admin_dir.as_ref().unwrap(), "config.d/20-b.toml", "");

    let settings = open(&roots);
    assert_eq!(settings.get("a"), Some(Value::Integer(3)));
    assert_eq!(settings.get("b"), Some(Value::Integer(0)));
    assert!(matches!(
        settings.resolved().origin("a"),
        Some(Origin::File {
            kind: LayerKind::Admin,
            ..
        })
    ));

    // A symlink to /dev/null masks it too.
    let masked = roots
        .admin_dir
        .as_ref()
        .unwrap()
        .join(NS)
        .join("config.d/10-a.toml");
    fs::remove_file(&masked).unwrap();
    std::os::unix::fs::symlink("/dev/null", &masked).unwrap();
    settings.reload();
    assert_eq!(settings.get("a"), Some(Value::Integer(0)));
}

#[test]
fn drop_ins_apply_after_every_system_conf_and_before_the_user() {
    let tmp = tempfile::tempdir().unwrap();
    let roots = Roots::under(tmp.path());
    put(
        roots.vendor_dir.as_ref().unwrap(),
        "config.d/10-a.toml",
        "a = 1\n",
    );
    put(
        roots.admin_dir.as_ref().unwrap(),
        "config.toml",
        "a = 2\nb = 2\n",
    );

    // A system drop-in overrides every system main file.
    let settings = open(&roots);
    assert_eq!(settings.get("a"), Some(Value::Integer(1)));
    assert_eq!(settings.get("b"), Some(Value::Integer(2)));

    put(roots.user_dir.as_ref().unwrap(), "config.toml", "a = 5\n");
    settings.reload();
    assert_eq!(settings.get("a"), Some(Value::Integer(5)));
}

#[test]
fn the_first_write_creates_a_private_user_root() {
    let tmp = tempfile::tempdir().unwrap();
    let user = tmp.path().join("fresh/user");
    let settings = common::load_in(&user, Schema::parse(SCHEMA).unwrap()).unwrap();
    settings.set("a", Value::Integer(1)).unwrap();
    for dir in [tmp.path().join("fresh"), user] {
        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "{}", dir.display());
    }
}

/// An app namespace has its own directory. A shared one is under mxconfig's.
#[test]
fn an_app_keeps_its_own_directory_and_a_shared_namespace_lives_in_mxconfigs() {
    let tmp = tempfile::tempdir().unwrap();
    let roots = Roots::under(tmp.path());
    let base = |kind: &str| tmp.path().join(kind);
    let app =
        Schema::parse("namespace = \"org.mechanix.demo\"\nversion = 1\ndir = \"mechanix-demo\"\n")
            .unwrap();
    let id = Schema::parse("namespace = \"org.mechanix.calc\"\nversion = 1\n").unwrap();
    let shared =
        Schema::parse("namespace = \"org.mechanix.desktop.appearance\"\nversion = 1\n").unwrap();

    let user = roots.user_dir.as_deref().unwrap();
    let admin = roots.admin_dir.as_deref().unwrap();
    assert_eq!(
        Home::of(user, &app).main_file(),
        base("user").join("mechanix-demo/config.toml")
    );
    assert_eq!(
        Home::of(admin, &app).drop_in_dir(),
        base("admin").join("mechanix-demo/config.d")
    );
    assert_eq!(
        Home::of(user, &id).dir,
        base("user").join("org.mechanix.calc"),
        "the id by default"
    );
    let home = Home::of(admin, &shared);
    assert_eq!(
        home.main_file(),
        base("admin").join("mxconfig/org.mechanix.desktop.appearance/conf.toml")
    );
    assert_eq!(
        home.bound,
        base("admin").join("mxconfig"),
        "a watch never climbs to the base"
    );

    // Only app namespaces may set `dir`, and only to a single name.
    let shared_dir = "namespace = \"org.mechanix.desktop.x\"\nversion = 1\ndir = \"x\"\n";
    assert!(Schema::parse(shared_dir).is_err());
    let nested = "namespace = \"org.mechanix.calc\"\nversion = 1\ndir = \"a/b\"\n";
    assert!(Schema::parse(nested).is_err());
}

/// A Flatpak app's files follow its deploy's `active` link.
#[test]
fn flatpak_roots_follow_the_active_deploy() {
    let tmp = tempfile::tempdir().unwrap();
    let branch = tmp.path().join("app/org.example.player/x86_64/stable");
    let (old, new) = (branch.join("aaa/files"), branch.join("bbb/files"));
    fs::create_dir_all(&old).unwrap();
    fs::create_dir_all(&new).unwrap();
    std::os::unix::fs::symlink("bbb", branch.join("active")).unwrap();

    let roots = Roots::flatpak(Path::new("/home/u/.var/app/org.example.player"), &old);
    let schemas = branch.join("active/files/share/mxconfig/schemas");
    assert_eq!(roots.schema_dirs[1], schemas);
    let config = Path::new("/home/u/.var/app/org.example.player/config");
    assert_eq!(roots.user_dir.as_deref(), Some(config));
    // `/app` is the app's `/usr`, so its `lib` is the vendor layer.
    let vendor = branch.join("active/files/lib");
    assert_eq!(roots.vendor_dir.as_deref(), Some(vendor.as_path()));

    // Without an `active` link, the given files are used.
    let bare = tmp.path().join("bare/files");
    let roots = Roots::flatpak(Path::new("/i"), &bare);
    assert_eq!(roots.schema_dirs[1], bare.join("share/mxconfig/schemas"));
    let vendor = bare.join("lib");
    assert_eq!(roots.vendor_dir.as_deref(), Some(vendor.as_path()));
}
