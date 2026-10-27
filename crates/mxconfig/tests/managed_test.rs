//! Tests for managed keys and assets, and for privileged namespaces.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};

use common::{ACCESS_DENIED, DAEMON, answer, changed, failed, members, open, owner, reach};
use common::{Through, int, put as write, reach_bus, remote, subscribed, text};
use mxconfig::host::Home;
use mxconfig::host::Roots;
use mxconfig::runtime::Drive;
use mxconfig::{Bus, Class, Edit, Error, ErrorKind, LayerKind, Origin, Outcome, Presence};
use mxconfig::{Schema, Settings, Source, Value};
use tempfile::TempDir;

/// An app namespace with managed and unmanaged keys and assets.
const APP: &str = r#"
namespace = "org.mechanix.demo"
version = 1

[assets.layout]
path = "ui/layout.yaml"
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
description = "Lock policy, unset by default."
managed = true

[keys.volume]
type = "integer"
default = 50
label = "Volume"
description = "Output volume."
"#;

/// A desktop namespace with one managed key.
const DESKTOP: &str = r#"
namespace = "org.mechanix.desktop.theme"
version = 1

[keys.accent]
type = "string"
default = "blue"
label = "Accent"
description = "The accent color."

[keys.policy]
type = "string"
default = "open"
label = "Policy"
description = "Theme policy."
managed = true
"#;

/// A privileged namespace with one managed key and an asset.
const PRIVILEGED: &str = r#"
namespace = "org.mechanix.privileged.display"
version = 1

[assets.curve]
path = "curve.txt"

[keys.brightness]
type = "integer"
default = 50
label = "Brightness"
description = "Panel brightness."

[keys.gamma]
type = "float"
default = 2.2
label = "Gamma"
description = "Panel gamma."
managed = true
"#;

/// An app namespace with nothing managed.
const PLAIN: &str = r#"
namespace = "org.mechanix.plain"
version = 1

[keys.volume]
type = "integer"
default = 50
label = "Volume"
description = "Output volume."
"#;

/// An app namespace whose only managed item is an asset.
const ASSET_ONLY: &str = r#"
namespace = "org.mechanix.assets"
version = 1

[assets.layout]
path = "layout.yaml"
managed = true

[keys.volume]
type = "integer"
default = 50
label = "Volume"
description = "Output volume."
"#;

const APP_NS: &str = "org.mechanix.demo";
const DESKTOP_NS: &str = "org.mechanix.desktop.theme";
const PRIVILEGED_NS: &str = "org.mechanix.privileged.display";

/// Every root under one temp dir.
struct Tree {
    tmp: TempDir,
    roots: Roots,
}

impl Tree {
    fn new() -> Tree {
        let tmp = TempDir::new().unwrap();
        let roots = Roots::under(tmp.path());
        fs::create_dir_all(&roots.schema_dirs[0]).unwrap();
        Tree { tmp, roots }
    }

    /// Install `schema` on the schema path.
    fn install(&self, schema: &str) {
        common::install(&self.roots, parse(schema).namespace(), schema);
    }

    /// The base directory of layer `kind`.
    fn root(&self, kind: LayerKind) -> &Path {
        let root = match kind {
            LayerKind::Vendor => &self.roots.vendor_dir,
            LayerKind::Admin => &self.roots.admin_dir,
            LayerKind::User => &self.roots.user_dir,
            other => panic!("no root for {other:?}"),
        };
        root.as_deref().unwrap()
    }

    /// `schema`'s home in layer `kind`.
    fn home(&self, kind: LayerKind, schema: &str) -> Home {
        Home::of(self.root(kind), &parse(schema))
    }

    /// `schema`'s main file in layer `kind`.
    fn file(&self, kind: LayerKind, schema: &str) -> PathBuf {
        self.home(kind, schema).main_file()
    }

    /// Write `text` as `schema`'s main file in layer `kind`.
    fn put(&self, kind: LayerKind, schema: &str, text: &str) -> PathBuf {
        write(&self.file(kind, schema), text)
    }

    /// Write `text` as drop-in `name` of `schema` in layer `kind`.
    fn drop_in(&self, kind: LayerKind, schema: &str, name: &str, text: &str) -> PathBuf {
        write(&self.home(kind, schema).drop_in_dir().join(name), text)
    }

    /// Write `text` as asset file `path` of `schema` in layer `kind`.
    fn asset(&self, kind: LayerKind, schema: &str, path: &str, text: &str) -> PathBuf {
        write(&self.home(kind, schema).dir.join(path), text)
    }

    fn source(&self) -> Source {
        Source::with_roots(self.roots.clone())
    }

    /// A source that serves clients on `bus`, as a daemon does.
    fn serving(&self, bus: Bus) -> Source {
        Source::with_roots(Roots {
            serving: Some(bus),
            ..self.roots.clone()
        })
    }

    /// Install `schema` and open it where its files are read.
    fn open(&self, schema: &str) -> Settings {
        self.install(schema);
        let schema = parse(schema);
        let source = match schema.class() {
            Class::App => self.source(),
            Class::Privileged => self.serving(Bus::System),
            Class::Desktop => self.serving(Bus::Session),
        };
        open(&source, schema.namespace()).unwrap()
    }

    /// Every directory and file in the tree, with file contents.
    fn snapshot(&self) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
        let mut found = BTreeMap::new();
        walk(self.tmp.path(), &mut found);
        found
    }
}

fn parse(schema: &str) -> Schema {
    Schema::parse(schema).unwrap()
}

/// Add every entry below `dir` to `found`. Directories map to `None`.
fn walk(dir: &Path, found: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.insert(path.clone(), None);
            walk(&path, found);
        } else {
            found.insert(path.clone(), Some(fs::read(&path).unwrap()));
        }
    }
}

/// Assert that `result` was refused as access denied.
#[track_caller]
fn denied<T: std::fmt::Debug>(result: Result<T, Error>) {
    let e = result.expect_err("the write is refused");
    assert_eq!(e.kind(), ErrorKind::Denied, "{e}");
}

/// Assert that nothing was queued on either bus and nothing was reported.
#[track_caller]
fn quiet(source: &Source) {
    let session = source.take_calls(Bus::Session);
    assert!(session.is_empty(), "{session:?}");
    let system = source.take_calls(Bus::System);
    assert!(system.is_empty(), "{system:?}");
    let outcomes = source.outcomes();
    assert!(outcomes.is_empty(), "{outcomes:?}");
}

/// The layer `key`'s value came from, if a file.
fn layer(settings: &Settings, key: &str) -> Option<LayerKind> {
    match settings.resolved().origin(key)? {
        Origin::File { kind, .. } => Some(*kind),
        _ => None,
    }
}

/// Whether a warning keyed to `key` says it is managed.
fn ignored(settings: &Settings, key: &str) -> bool {
    let diags = settings.diagnostics();
    diags
        .iter()
        .any(|d| !d.is_error() && d.key() == Some(key) && d.message().contains("managed"))
}

/// The warnings that a system file is writable by the current user.
fn writable_notes(settings: &Settings) -> Vec<String> {
    let diags = settings.diagnostics().into_iter();
    let notes = diags.filter(|d| d.message().contains("writable by the current user"));
    notes.map(|d| d.message().to_string()).collect()
}

/// A read-only fd on a new file holding `bytes`, outside any tree.
fn fd_of(bytes: &[u8]) -> (TempDir, OwnedFd) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("asset");
    fs::write(&path, bytes).unwrap();
    (dir, fs::File::open(&path).unwrap().into())
}

// Writes of managed keys and assets.

/// A managed key's set and remove are denied and change nothing.
#[test]
fn a_managed_key_write_is_denied_and_touches_nothing() {
    let tree = Tree::new();
    tree.put(LayerKind::Admin, APP, "brightness = 20\n");
    let settings = tree.open(APP);
    let source = settings.source().clone();
    reach(&source);
    reach_bus(&source, Bus::System);
    drop(source.outcomes());
    let before = tree.snapshot();

    denied(settings.write(&[Edit::set("brightness", 1)]));
    denied(settings.write(&[Edit::remove("brightness")]));
    denied(settings.write(&[Edit::set("lock", "on")]));
    denied(settings.set("brightness", int(1)));
    denied(settings.remove("lock"));

    assert_eq!(tree.snapshot(), before, "nothing was written");
    quiet(&source);
    assert_eq!(settings.get("brightness"), Some(int(20)));
    assert_eq!(settings.revision(), 0);
}

/// A batch with a managed key is denied whole, in either order.
#[test]
fn a_mixed_batch_with_a_managed_key_writes_nothing() {
    let tree = Tree::new();
    let settings = tree.open(APP);
    let source = settings.source().clone();
    reach(&source);
    let before = tree.snapshot();

    let first = [Edit::set("volume", 10), Edit::set("brightness", 10)];
    denied(settings.write(&first));
    let last = [Edit::set("brightness", 10), Edit::remove("volume")];
    denied(settings.write(&last));
    let middle = [
        Edit::set("volume", 10),
        Edit::remove("lock"),
        Edit::set("volume", 11),
    ];
    denied(settings.apply(&middle));

    assert_eq!(tree.snapshot(), before);
    quiet(&source);
    assert_eq!(settings.get("volume"), Some(int(50)), "no edit applied");
    assert!(!tree.file(LayerKind::User, APP).exists());
}

/// The refusal names the managed key.
#[test]
fn a_managed_refusal_names_the_key() {
    let tree = Tree::new();
    let settings = tree.open(APP);
    let e = settings.write(&[Edit::set("brightness", 1)]).unwrap_err();
    assert!(e.to_string().contains("brightness"), "{e}");
    assert_ne!(e.kind(), ErrorKind::Invalid, "{e}");
    assert!(!matches!(e.kind(), ErrorKind::Unreachable(_)));
}

/// A managed key with a bad value is refused too, and nothing is written.
#[test]
fn a_managed_key_with_a_bad_value_is_still_refused() {
    let tree = Tree::new();
    let settings = tree.open(APP);
    let before = tree.snapshot();
    assert!(settings.write(&[Edit::set("brightness", "high")]).is_err());
    assert_eq!(tree.snapshot(), before);
    quiet(settings.source());
}

/// A managed write is denied even when there is no user dir to write.
#[test]
fn a_managed_write_is_denied_without_a_user_dir() {
    let tree = Tree::new();
    tree.install(APP);
    let source = Source::with_roots(Roots {
        user_dir: None,
        ..tree.roots.clone()
    });
    let settings = open(&source, APP_NS).unwrap();
    denied(settings.write(&[Edit::set("brightness", 1)]));
    denied(settings.write_asset("layout", b"x"));
    // An unmanaged key fails for the missing dir instead.
    let e = settings.set("volume", int(1)).unwrap_err();
    assert_ne!(e.kind(), ErrorKind::Denied, "{e}");
}

/// An unmanaged key next to managed ones writes the user file.
#[test]
fn an_unmanaged_key_beside_managed_ones_still_writes() {
    let tree = Tree::new();
    let settings = tree.open(APP);
    settings.set("volume", int(10)).unwrap();
    assert_eq!(settings.get("volume"), Some(int(10)));
    assert_eq!(layer(&settings, "volume"), Some(LayerKind::User));
    let written = fs::read_to_string(tree.file(LayerKind::User, APP)).unwrap();
    assert!(written.contains("volume = 10"), "{written}");
    assert!(!written.contains("brightness"), "{written}");
}

/// A managed asset refuses every asset writer and creates no file.
#[test]
fn a_managed_asset_refuses_every_writer() {
    let tree = Tree::new();
    tree.asset(LayerKind::User, APP, "ui/layout.yaml", "user");
    let settings = tree.open(APP);
    let source = settings.source().clone();
    reach(&source);
    let before = tree.snapshot();

    denied(settings.write_asset("layout", b"x"));
    let (_dir, fd) = fd_of(b"from a file");
    denied(settings.write_asset_from("layout", fd));
    denied(settings.clear_asset("layout"));

    assert_eq!(tree.snapshot(), before, "the user copy is left alone");
    quiet(&source);
}

/// An unmanaged asset in the same namespace is written as usual.
#[test]
fn an_unmanaged_asset_beside_a_managed_one_still_writes() {
    let tree = Tree::new();
    let settings = tree.open(APP);
    settings.set_asset("theme", b"body {}").unwrap();
    let asset = settings.asset("theme").unwrap();
    assert_eq!(asset.text().unwrap(), "body {}");
    assert!(matches!(
        asset.origin,
        Origin::File {
            kind: LayerKind::User,
            ..
        }
    ));
    settings.remove_asset("theme").unwrap();
    assert!(settings.asset("theme").is_none());
}

/// A managed key in a desktop namespace is denied like anywhere.
#[test]
fn a_managed_key_in_a_desktop_namespace_is_denied() {
    let tree = Tree::new();
    let settings = tree.open(DESKTOP);
    let before = tree.snapshot();
    denied(settings.write(&[Edit::set("policy", "closed")]));
    denied(settings.write(&[Edit::set("accent", "red"), Edit::remove("policy")]));
    assert_eq!(tree.snapshot(), before);
    quiet(settings.source());
}

// Reading managed keys and assets.

/// A managed key takes vendor, then admin. Never the user's.
#[test]
fn a_managed_key_takes_vendor_then_admin_values() {
    let tree = Tree::new();
    tree.put(LayerKind::Vendor, APP, "brightness = 10\n");
    tree.put(LayerKind::User, APP, "brightness = 99\n");
    let settings = tree.open(APP);
    assert_eq!(settings.get("brightness"), Some(int(10)));
    assert_eq!(layer(&settings, "brightness"), Some(LayerKind::Vendor));

    tree.put(LayerKind::Admin, APP, "brightness = 20\n");
    settings.reload();
    assert_eq!(settings.get("brightness"), Some(int(20)));
    assert_eq!(layer(&settings, "brightness"), Some(LayerKind::Admin));
    assert!(ignored(&settings, "brightness"));
}

/// The user's value for a managed key is ignored with a keyed warning.
#[test]
fn a_user_value_for_a_managed_key_is_ignored_with_a_warning() {
    let tree = Tree::new();
    tree.put(LayerKind::Admin, APP, "brightness = 20\n");
    let user = tree.put(LayerKind::User, APP, "brightness = 99\nlock = \"x\"\n");
    let settings = tree.open(APP);

    assert_eq!(settings.get("brightness"), Some(int(20)));
    assert_eq!(
        settings.get("lock"),
        None,
        "unset, as no system layer sets it"
    );
    let diags = settings.diagnostics();
    let note = diags
        .iter()
        .find(|d| d.key() == Some("brightness"))
        .expect("a note for brightness");
    assert!(!note.is_error(), "{note:?}");
    assert_eq!(note.source(), Some(user.display().to_string().as_str()));
    assert!(ignored(&settings, "lock"));
}

/// A user drop-in can't set a managed key, but an admin drop-in can.
#[test]
fn a_user_drop_in_for_a_managed_key_is_ignored() {
    let tree = Tree::new();
    tree.put(LayerKind::Admin, APP, "brightness = 20\n");
    let user = tree.drop_in(LayerKind::User, APP, "10-mine.toml", "brightness = 99\n");
    let settings = tree.open(APP);
    assert_eq!(settings.get("brightness"), Some(int(20)));
    let diags = settings.diagnostics();
    let from_drop_in = |d: &&mxconfig::Diagnostic| {
        d.key() == Some("brightness") && d.source() == Some(&*user.display().to_string())
    };
    assert!(diags.iter().any(|d| from_drop_in(&d)), "{diags:?}");

    tree.drop_in(LayerKind::Admin, APP, "50-site.toml", "brightness = 30\n");
    settings.reload();
    assert_eq!(settings.get("brightness"), Some(int(30)));
    assert_eq!(layer(&settings, "brightness"), Some(LayerKind::Admin));
}

/// A managed key with no layer value uses its schema default.
#[test]
fn a_managed_key_without_a_layer_value_uses_its_default() {
    let tree = Tree::new();
    tree.put(LayerKind::User, APP, "brightness = 99\n");
    let settings = tree.open(APP);
    assert_eq!(settings.get("brightness"), Some(int(50)));
    assert_eq!(
        settings.resolved().origin("brightness"),
        Some(&Origin::Schema)
    );
    // A managed key without a default stays unset.
    assert_eq!(settings.get("lock"), None);
}

/// Edits to the user file never change a managed key. Admin edits do.
#[test]
fn a_reload_reports_admin_but_not_user_changes_to_a_managed_key() {
    let tree = Tree::new();
    tree.put(LayerKind::Admin, APP, "brightness = 20\n");
    let settings = tree.open(APP);

    tree.put(LayerKind::User, APP, "brightness = 99\n");
    let changes = settings.reload();
    assert!(changes.is_empty(), "{changes:?}");
    assert_eq!(settings.get("brightness"), Some(int(20)));

    tree.put(LayerKind::Admin, APP, "brightness = 25\n");
    let changes = settings.reload();
    let keys: Vec<&str> = changes.iter().map(|c| c.key.as_str()).collect();
    assert_eq!(keys, ["brightness"]);
    assert_eq!(changes[0].value, Some(int(25)));
}

/// Unmanaged keys in a namespace with managed ones layer as usual.
#[test]
fn an_unmanaged_key_beside_managed_ones_layers_as_usual() {
    let tree = Tree::new();
    tree.put(LayerKind::Admin, APP, "brightness = 20\nvolume = 30\n");
    tree.put(LayerKind::User, APP, "brightness = 99\nvolume = 70\n");
    let settings = tree.open(APP);
    assert_eq!(settings.get("volume"), Some(int(70)));
    assert_eq!(layer(&settings, "volume"), Some(LayerKind::User));
    assert!(!ignored(&settings, "volume"));
    assert_eq!(settings.get("brightness"), Some(int(20)));
}

/// A managed asset ignores the user's copy. An unmanaged one uses it.
#[test]
fn a_managed_asset_ignores_the_user_copy_but_an_unmanaged_one_uses_it() {
    let tree = Tree::new();
    tree.asset(LayerKind::Vendor, APP, "ui/layout.yaml", "vendor");
    tree.asset(LayerKind::User, APP, "ui/layout.yaml", "user");
    tree.asset(LayerKind::Admin, APP, "theme.css", "admin");
    tree.asset(LayerKind::User, APP, "theme.css", "user");
    let settings = tree.open(APP);

    let layout = settings.asset("layout").unwrap();
    assert_eq!(layout.text().unwrap(), "vendor");
    assert!(matches!(
        layout.origin,
        Origin::File {
            kind: LayerKind::Vendor,
            ..
        }
    ));
    assert_eq!(settings.asset("theme").unwrap().text().unwrap(), "user");

    // The admin copy outranks the vendor one.
    tree.asset(LayerKind::Admin, APP, "ui/layout.yaml", "admin");
    assert_eq!(settings.asset("layout").unwrap().text().unwrap(), "admin");
}

/// A managed asset with only a user copy is unset.
#[test]
fn a_managed_asset_with_only_a_user_copy_is_unset() {
    let tree = Tree::new();
    tree.asset(LayerKind::User, APP, "ui/layout.yaml", "user");
    let settings = tree.open(APP);
    assert!(settings.asset("layout").is_none());
}

/// A managed key in a desktop namespace ignores the user. Others don't.
#[test]
fn a_desktop_namespace_ignores_the_user_only_for_managed_keys() {
    let tree = Tree::new();
    tree.put(
        LayerKind::Admin,
        DESKTOP,
        "accent = \"green\"\npolicy = \"strict\"\n",
    );
    tree.put(
        LayerKind::User,
        DESKTOP,
        "accent = \"red\"\npolicy = \"loose\"\n",
    );
    let settings = tree.open(DESKTOP);
    assert_eq!(settings.get("accent"), Some(text("red")));
    assert_eq!(layer(&settings, "accent"), Some(LayerKind::User));
    assert_eq!(settings.get("policy"), Some(text("strict")));
    assert!(ignored(&settings, "policy"));
}

/// `validate` flags an env var on a managed key or asset.
#[test]
fn validate_flags_env_on_a_managed_key_or_asset() {
    let schema = APP
        .replace(
            "description = \"Panel brightness.\"\n",
            "description = \"Panel brightness.\"\nenv = \"MXCONFIG_TEST_BRIGHTNESS\"\n",
        )
        .replace(
            "path = \"ui/layout.yaml\"\n",
            "path = \"ui/layout.yaml\"\nenv = \"MXCONFIG_TEST_LAYOUT\"\n",
        )
        .replace(
            "description = \"Output volume.\"\n",
            "description = \"Output volume.\"\nenv = \"MXCONFIG_TEST_VOLUME\"\n",
        );
    let schema = parse(&schema);
    let brightness = schema.key("brightness").unwrap();
    assert_eq!(brightness.env_var(), None);
    assert!(schema.key("volume").unwrap().env_var().is_some());
    assert_eq!(schema.asset("layout").unwrap().env_var(), None);

    let problems: Vec<String> = schema
        .validate()
        .iter()
        .map(|d| d.message().to_string())
        .collect();
    let flagged = |owner: &str| {
        let needle = format!("`{owner}` is managed, so its env var is never read");
        problems.iter().any(|p| p == &needle)
    };
    assert!(flagged("brightness"), "{problems:?}");
    assert!(flagged("layout"), "{problems:?}");
    assert!(!flagged("volume"), "{problems:?}");
    assert_eq!(problems.len(), 2, "{problems:?}");
}

/// `has_managed` counts managed keys and managed assets.
#[test]
fn has_managed_counts_keys_and_assets() {
    assert!(parse(APP).has_managed());
    assert!(parse(ASSET_ONLY).has_managed());
    assert!(parse(DESKTOP).has_managed());
    assert!(!parse(PLAIN).has_managed());
    let unmanaged = PRIVILEGED.replace("managed = true\n", "");
    assert!(!parse(&unmanaged).has_managed());
}

// Privileged namespaces.

/// A privileged namespace reads vendor and admin, never the user.
#[test]
fn a_privileged_namespace_reads_only_system_layers() {
    let tree = Tree::new();
    tree.put(LayerKind::Vendor, PRIVILEGED, "brightness = 10\n");
    tree.put(LayerKind::User, PRIVILEGED, "brightness = 99\n");
    tree.drop_in(
        LayerKind::User,
        PRIVILEGED,
        "10-mine.toml",
        "brightness = 98\n",
    );
    let settings = tree.open(PRIVILEGED);
    assert_eq!(settings.get("brightness"), Some(int(10)));

    tree.put(
        LayerKind::Admin,
        PRIVILEGED,
        "brightness = 20\ngamma = 1.8\n",
    );
    settings.reload();
    assert_eq!(settings.get("brightness"), Some(int(20)));
    assert_eq!(settings.get("gamma"), Some(Value::Float(1.8)));
    // User files aren't read at all, so they draw no diagnostic.
    let user = tree.root(LayerKind::User).display().to_string();
    let diags = settings.diagnostics();
    let from_user = diags
        .iter()
        .filter(|d| d.source().is_some_and(|s| s.starts_with(&user)));
    assert_eq!(from_user.count(), 0, "{diags:?}");
}

/// A privileged namespace ignores the user's asset copy.
#[test]
fn a_privileged_asset_ignores_the_user_copy() {
    let tree = Tree::new();
    tree.asset(LayerKind::User, PRIVILEGED, "curve.txt", "user");
    let settings = tree.open(PRIVILEGED);
    assert!(settings.asset("curve").is_none());
    tree.asset(LayerKind::Admin, PRIVILEGED, "curve.txt", "admin");
    assert_eq!(settings.asset("curve").unwrap().text().unwrap(), "admin");
}

/// A privileged namespace neither creates nor watches the user dir.
#[test]
fn a_privileged_namespace_never_watches_the_user_dir() {
    let tree = Tree::new();
    tree.put(LayerKind::Admin, PRIVILEGED, "brightness = 20\n");
    let settings = tree.open(PRIVILEGED);
    let user = tree.root(LayerKind::User);
    let dirs = settings.source().take_watch_dirs().unwrap();
    assert!(!dirs.is_empty(), "the system layers are watched");
    assert!(dirs.iter().all(|d| !d.starts_with(user)), "{dirs:?}");
    assert!(!user.exists(), "no user home is made");
}

/// A desktop namespace keeps its user layer, home and watch.
#[test]
fn a_desktop_namespace_keeps_its_user_layer() {
    let tree = Tree::new();
    tree.put(LayerKind::Admin, DESKTOP, "accent = \"green\"\n");
    let settings = tree.open(DESKTOP);
    let source = settings.source().clone();
    let user = tree.root(LayerKind::User);
    let dirs = source.take_watch_dirs().unwrap();
    assert!(dirs.iter().any(|d| d.starts_with(user)), "{dirs:?}");
    assert!(tree.home(LayerKind::User, DESKTOP).bound.is_dir());

    reach_bus(&source, Bus::System);
    drop(source.outcomes());
    settings.set("accent", text("red")).unwrap();
    assert_eq!(settings.get("accent"), Some(text("red")));
    let written = fs::read_to_string(tree.file(LayerKind::User, DESKTOP)).unwrap();
    assert!(written.contains("accent = \"red\""), "{written}");
    assert!(source.take_calls(Bus::System).is_empty());
    assert!(source.take_calls(Bus::Session).is_empty());
}

/// A client holds a shared namespace's defaults until its daemon serves it.
#[test]
fn a_client_never_opens_a_shared_namespace_from_files() {
    let tree = Tree::new();
    let shared = [
        (DESKTOP, DESKTOP_NS, Bus::Session),
        (PRIVILEGED, PRIVILEGED_NS, Bus::System),
    ];
    for (schema, namespace, bus) in shared {
        tree.install(schema);
        // A file the client must not read.
        tree.put(
            LayerKind::Admin,
            schema,
            "accent = \"red\"\nbrightness = 1\n",
        );
        let source = tree.source();
        let settings = open(&source, namespace).unwrap();
        assert!(settings.on_defaults() && !settings.from_daemon());
        let defaults = parse(schema)
            .keys()
            .iter()
            .filter(|k| k.default.is_some())
            .count();
        assert_eq!(
            settings.resolved().iter().count(),
            defaults,
            "no file was read"
        );
        assert_eq!(source.daemon(bus), Presence::NoBus);
        // Reaching the bus asks the daemon.
        let calls = reach_bus(&source, bus);
        assert_eq!(source.daemon(bus), Presence::Running);
        assert_eq!(members(&calls), ["Subscribe"]);
    }
}

/// A managed key in a privileged namespace is denied before any call.
#[test]
fn a_managed_key_in_a_privileged_namespace_is_denied() {
    let tree = Tree::new();
    let settings = tree.open(PRIVILEGED);
    let source = settings.source().clone();
    reach_bus(&source, Bus::System);
    denied(settings.write(&[Edit::set("gamma", 1.0)]));
    denied(settings.write(&[Edit::set("brightness", 1), Edit::remove("gamma")]));
    quiet(&source);
}

/// A privileged namespace refuses asset writes, reachable bus or not.
#[test]
fn a_privileged_namespace_refuses_asset_writes() {
    let tree = Tree::new();
    let settings = tree.open(PRIVILEGED);
    let source = settings.source().clone();
    for reached in [false, true] {
        if reached {
            reach_bus(&source, Bus::System);
            reach(&source);
        }
        denied(settings.write_asset("curve", b"x"));
        let (_dir, fd) = fd_of(b"x");
        denied(settings.write_asset_from("curve", fd));
        denied(settings.clear_asset("curve"));
        quiet(&source);
    }
    assert!(!tree.root(LayerKind::User).exists());
}

// Serving sources.

/// The privileged daemon's source opens only privileged namespaces.
#[test]
fn a_system_serving_source_opens_only_privileged_namespaces() {
    let tree = Tree::new();
    for schema in [APP, DESKTOP, PRIVILEGED] {
        tree.install(schema);
    }
    let source = tree.serving(Bus::System);
    for namespace in [APP_NS, DESKTOP_NS] {
        let e = open(&source, namespace).unwrap_err();
        assert!(e.to_string().contains("served by mxconfigd"), "{e}");
    }
    let e = source.open_schema(parse(PLAIN)).unwrap_err();
    assert!(e.to_string().contains("served by mxconfigd"), "{e}");
    assert!(open(&source, PRIVILEGED_NS).is_ok());
}

/// The privileged daemon writes the admin file, never a user file.
#[test]
fn a_system_serving_source_writes_the_admin_file() {
    let tree = Tree::new();
    tree.install(PRIVILEGED);
    tree.put(LayerKind::User, PRIVILEGED, "brightness = 99\n");
    let source = tree.serving(Bus::System);
    let settings = open(&source, PRIVILEGED_NS).unwrap();
    assert_eq!(settings.get("brightness"), Some(int(50)));
    let user_before = fs::read(tree.file(LayerKind::User, PRIVILEGED)).unwrap();

    settings.set("brightness", int(70)).unwrap();
    let admin = tree.file(LayerKind::Admin, PRIVILEGED);
    let expected = tree
        .root(LayerKind::Admin)
        .join("mxconfig")
        .join(PRIVILEGED_NS)
        .join("conf.toml");
    assert_eq!(admin, expected);
    let written = fs::read_to_string(&admin).unwrap();
    assert!(written.contains("brightness = 70"), "{written}");
    assert_eq!(settings.get("brightness"), Some(int(70)));
    assert_eq!(layer(&settings, "brightness"), Some(LayerKind::Admin));
    let user_after = fs::read(tree.file(LayerKind::User, PRIVILEGED)).unwrap();
    assert_eq!(user_after, user_before, "the user file is left alone");
    assert!(source.take_calls(Bus::System).is_empty());
    assert!(source.take_calls(Bus::Session).is_empty());

    settings.remove("brightness").unwrap();
    assert_eq!(settings.get("brightness"), Some(int(50)));
    let written = fs::read_to_string(&admin).unwrap();
    assert!(!written.contains("brightness"), "{written}");
}

/// The privileged daemon still refuses managed keys and assets.
#[test]
fn a_system_serving_source_refuses_managed_keys_and_assets() {
    let tree = Tree::new();
    tree.install(PRIVILEGED);
    let source = tree.serving(Bus::System);
    let settings = open(&source, PRIVILEGED_NS).unwrap();
    let before = tree.snapshot();
    denied(settings.write(&[Edit::set("gamma", 1.0)]));
    denied(settings.write(&[Edit::set("brightness", 1), Edit::set("gamma", 1.0)]));
    denied(settings.write_asset("curve", b"x"));
    denied(settings.clear_asset("curve"));
    assert_eq!(tree.snapshot(), before);
    quiet(&source);
}

/// The session daemon's source refuses managed keys and writes the rest.
#[test]
fn a_session_serving_source_refuses_managed_keys() {
    let tree = Tree::new();
    tree.install(APP);
    let source = tree.serving(Bus::Session);
    let settings = open(&source, APP_NS).unwrap();
    let before = tree.snapshot();
    denied(settings.write(&[Edit::set("brightness", 1)]));
    denied(settings.write_asset("layout", b"x"));
    assert_eq!(tree.snapshot(), before);
    quiet(&source);

    settings.set("volume", int(10)).unwrap();
    let written = fs::read_to_string(tree.file(LayerKind::User, APP)).unwrap();
    assert!(written.contains("volume = 10"), "{written}");
}

/// The session daemon's source leaves privileged namespaces to the privileged one.
#[test]
fn a_session_serving_source_opens_no_privileged_namespace() {
    let tree = Tree::new();
    tree.install(PRIVILEGED);
    let e = open(&tree.serving(Bus::Session), PRIVILEGED_NS).unwrap_err();
    assert!(
        e.to_string().contains("served by the privileged service"),
        "{e}"
    );
}

// Migrations.

/// A user file migrates in a namespace with managed keys.
#[test]
fn a_namespace_with_managed_keys_still_migrates() {
    let schema = r#"
namespace = "org.mechanix.demo"
version = 2

[keys.brightness]
type = "integer"
default = 50
label = "Brightness"
description = "Panel brightness."
managed = true

[keys.volume]
type = "integer"
default = 50
label = "Volume"
description = "Output volume."

[[migration]]
from = 1
to = 2
ops = [{ op = "rename", from = "level", to = "volume" }]
"#;
    let tree = Tree::new();
    tree.put(LayerKind::Admin, schema, "brightness = 20\n");
    let user = tree.put(LayerKind::User, schema, "brightness = 99\nlevel = 7\n");
    write(&user.with_file_name("config.toml.state"), "version = 1\n");

    let settings = tree.open(schema);
    assert_eq!(settings.get("volume"), Some(int(7)));
    assert_eq!(settings.get("brightness"), Some(int(20)));
    assert!(ignored(&settings, "brightness"));
    let migrated = fs::read_to_string(&user).unwrap();
    assert!(migrated.contains("volume = 7"), "{migrated}");
    assert!(!migrated.contains("level"), "{migrated}");
    let state = fs::read_to_string(user.with_file_name("config.toml.state")).unwrap();
    assert!(state.contains("version = 2"), "{state}");
}

/// A migration that renames into a managed key leaves the value ignored.
#[test]
fn a_rename_into_a_managed_key_stays_ignored() {
    let schema = r#"
namespace = "org.mechanix.demo"
version = 2

[keys.brightness]
type = "integer"
default = 50
label = "Brightness"
description = "Panel brightness."
managed = true

[[migration]]
from = 1
to = 2
ops = [{ op = "rename", from = "level", to = "brightness" }]
"#;
    let tree = Tree::new();
    let user = tree.put(LayerKind::User, schema, "level = 99\n");
    write(&user.with_file_name("config.toml.state"), "version = 1\n");
    let settings = tree.open(schema);
    assert_eq!(settings.get("brightness"), Some(int(50)));
    assert!(ignored(&settings, "brightness"));
}

/// A privileged schema, at v2, that renamed `level` to `brightness`.
const PRIVILEGED_V2: &str = r#"
namespace = "org.mechanix.privileged.display"
version = 2

[keys.brightness]
type = "integer"
default = 50
label = "Brightness"
description = "Panel brightness."

[[migration]]
from = 1
to = 2
ops = [{ op = "rename", from = "level", to = "brightness" }]
"#;

/// The privileged daemon migrates the admin file.
#[test]
fn a_system_serving_source_migrates_the_admin_file() {
    let tree = Tree::new();
    tree.install(PRIVILEGED_V2);
    let admin = tree.put(LayerKind::Admin, PRIVILEGED_V2, "level = 30\n");
    write(&admin.with_file_name("conf.toml.state"), "version = 1\n");

    let source = tree.serving(Bus::System);
    let settings = open(&source, PRIVILEGED_NS).unwrap();
    assert_eq!(settings.get("brightness"), Some(int(30)));
    let migrated = fs::read_to_string(&admin).unwrap();
    assert!(migrated.contains("brightness = 30"), "{migrated}");
    assert!(!migrated.contains("level"), "{migrated}");
    assert_eq!(common::recorded(&admin).unwrap().0, 2);
    assert!(!tree.root(LayerKind::User).exists());
}

// Diagnostics for system files the user can write.

/// Each system file the user can write draws a warning, for managed keys.
#[test]
fn a_writable_system_file_warns_for_a_namespace_with_managed_keys() {
    let tree = Tree::new();
    let vendor = tree.put(LayerKind::Vendor, APP, "brightness = 10\n");
    let admin = tree.put(LayerKind::Admin, APP, "brightness = 20\n");
    let drop_in = tree.drop_in(LayerKind::Admin, APP, "50-site.toml", "volume = 1\n");
    tree.put(LayerKind::User, APP, "volume = 2\n");
    let settings = tree.open(APP);

    let notes = writable_notes(&settings);
    assert_eq!(notes.len(), 3, "{notes:?}");
    for file in [vendor, admin, drop_in] {
        let file = file.display().to_string();
        assert!(
            notes.iter().any(|n| n.starts_with(&file)),
            "{file}: {notes:?}"
        );
    }
    let warned = settings.diagnostics().into_iter();
    let warned = warned.filter(|d| d.message().contains("writable by the current user"));
    assert!(warned.into_iter().all(|d| !d.is_error()));
}

/// A writable admin file draws a warning in a privileged namespace.
#[test]
fn a_writable_system_file_warns_for_a_privileged_namespace() {
    let unmanaged = PRIVILEGED.replace("managed = true\n", "");
    let tree = Tree::new();
    let admin = tree.put(LayerKind::Admin, &unmanaged, "brightness = 20\n");
    let settings = tree.open(&unmanaged);
    let notes = writable_notes(&settings);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0].starts_with(&admin.display().to_string()),
        "{notes:?}"
    );
}

/// A managed asset alone is enough for the warning.
#[test]
fn a_writable_system_file_warns_for_a_managed_asset() {
    let tree = Tree::new();
    tree.put(LayerKind::Admin, ASSET_ONLY, "volume = 20\n");
    let settings = tree.open(ASSET_ONLY);
    assert_eq!(writable_notes(&settings).len(), 1);
}

/// A namespace with nothing managed and not privileged draws no warning.
#[test]
fn a_namespace_with_nothing_managed_gets_no_writable_warning() {
    let tree = Tree::new();
    tree.put(LayerKind::Admin, PLAIN, "volume = 20\n");
    let settings = tree.open(PLAIN);
    assert_eq!(writable_notes(&settings), Vec::<String>::new());

    let desktop = DESKTOP.replace("managed = true\n", "");
    tree.put(LayerKind::Admin, &desktop, "accent = \"green\"\n");
    let settings = tree.open(&desktop);
    assert_eq!(writable_notes(&settings), Vec::<String>::new());
}

/// The user's own files never draw the warning.
#[test]
fn a_user_file_never_draws_the_writable_warning() {
    let tree = Tree::new();
    tree.put(LayerKind::User, APP, "volume = 2\n");
    tree.drop_in(LayerKind::User, APP, "10-mine.toml", "volume = 3\n");
    let settings = tree.open(APP);
    assert_eq!(writable_notes(&settings), Vec::<String>::new());
}

// Daemon-backed handles.

/// Open `namespace` through a fake daemon on its bus that serves `schema`.
fn served(source: &Source, namespace: &str, schema: &str, values: &[(&str, Value)]) -> Settings {
    let bus = Bus::serving(Class::of(namespace).unwrap());
    reach_bus(source, bus);
    let ticket = source.request_namespace(namespace);
    let calls = source.take_calls(bus);
    let [call] = &*calls else { panic!("{calls:?}") };
    assert_eq!(call.member, "Subscribe");
    let values = values.iter().map(|(k, v)| (k.to_string(), v.clone()));
    let args = subscribed(schema, 1, &values.collect(), &[]);
    source.deliver(bus, answer(call.id, args));
    let settings = common::opened(source, ticket).unwrap();
    assert!(settings.from_daemon());
    settings
}

/// A daemon-backed handle refuses managed writes before calling the daemon.
#[test]
fn a_daemon_backed_handle_refuses_managed_writes_locally() {
    let tree = Tree::new();
    tree.install(&remote(APP));
    let source = tree.source();
    let values = [("brightness", int(20)), ("volume", int(50))];
    let settings = served(&source, APP_NS, APP, &values);
    reach_bus(&source, Bus::System);
    assert_eq!(settings.get("brightness"), Some(int(20)));

    denied(settings.write(&[Edit::set("brightness", 1)]));
    denied(settings.write(&[Edit::set("volume", 1), Edit::remove("lock")]));
    denied(settings.write_asset("layout", b"x"));
    let (_dir, fd) = fd_of(b"x");
    denied(settings.write_asset_from("layout", fd));
    denied(settings.clear_asset("layout"));
    quiet(&source);

    // An unmanaged key goes to the session daemon.
    settings.write(&[Edit::set("volume", 1)]).unwrap();
    assert_eq!(members(&source.take_calls(Bus::Session)), ["Apply"]);
    assert!(source.take_calls(Bus::System).is_empty());
}

/// A daemon-backed privileged handle writes through the system bus.
#[test]
fn a_daemon_backed_privileged_handle_writes_through_the_system_bus() {
    let tree = Tree::new();
    let source = tree.source();
    let settings = served(&source, PRIVILEGED_NS, PRIVILEGED, &[]);
    denied(settings.write(&[Edit::set("gamma", 1.0)]));
    denied(settings.write_asset("curve", b"x"));
    denied(settings.clear_asset("curve"));
    quiet(&source);

    settings.write(&[Edit::set("brightness", 1)]).unwrap();
    assert_eq!(members(&source.take_calls(Bus::System)), ["Apply"]);
    assert!(source.take_calls(Bus::Session).is_empty());
    let outcomes = source.outcomes();
    let written = outcomes
        .iter()
        .any(|o| matches!(o, Outcome::Written { .. } | Outcome::WriteFailed { .. }));
    assert!(!written, "it waits for the system daemon: {outcomes:?}");
}

/// A daemon-backed desktop handle refuses its managed key too.
#[test]
fn a_daemon_backed_desktop_handle_refuses_managed_keys() {
    let tree = Tree::new();
    let source = tree.source();
    let settings = served(&source, DESKTOP_NS, DESKTOP, &[]);
    denied(settings.write(&[Edit::set("policy", "closed")]));
    quiet(&source);
    settings.write(&[Edit::set("accent", "red")]).unwrap();
    assert_eq!(members(&source.take_calls(Bus::Session)), ["Apply"]);
}

/// A privileged handle hears only the privileged daemon.
#[test]
fn a_privileged_handle_hears_only_the_system_daemon() {
    let tree = Tree::new();
    let source = tree.source();
    reach(&source);
    let settings = served(&source, PRIVILEGED_NS, PRIVILEGED, &[]);
    let values = BTreeMap::from([("brightness".into(), int(7))]);
    source.deliver(Bus::Session, changed(PRIVILEGED_NS, 2, &values));
    assert_eq!(settings.get("brightness"), None, "the snapshot had none");
    source.deliver(Bus::System, changed(PRIVILEGED_NS, 2, &values));
    assert_eq!(settings.get("brightness"), Some(int(7)));
}

/// A defaults handle follows the daemon once it answers, as a change.
#[test]
fn a_defaults_handle_follows_the_daemon_once_it_answers() {
    let tree = Tree::new();
    tree.install(DESKTOP);
    let source = tree.source();
    let settings = open(&source, DESKTOP_NS).unwrap();
    let calls = reach_bus(&source, Bus::Session);
    let [call] = &*calls else { panic!("{calls:?}") };
    let values = BTreeMap::from([("accent".into(), text("red"))]);
    let reply = subscribed(DESKTOP, 1, &values, &[]);
    source.deliver(Bus::Session, answer(call.id, reply));
    assert!(!settings.on_defaults() && settings.from_daemon());
    assert_eq!(settings.get("accent"), Some(text("red")));
    let changed = |o: &Outcome| matches!(o, Outcome::Changed(c) if c.key == "accent");
    assert!(source.outcomes().iter().any(changed));
}

/// A shared open without a daemon follows one that starts later.
#[test]
fn a_missing_daemon_leaves_the_defaults_until_one_starts() {
    let tree = Tree::new();
    tree.install(DESKTOP);
    let source = tree.source();
    reach(&source);
    let ticket = source.request_namespace(DESKTOP_NS);
    let calls = source.take_calls(Bus::Session);
    let [call] = &*calls else { panic!("{calls:?}") };
    source.deliver(
        Bus::Session,
        failed(call.id, "org.freedesktop.DBus.Error.ServiceUnknown"),
    );
    let settings = common::opened(&source, ticket).unwrap();
    assert!(settings.on_defaults());
    assert!(
        source.take_calls(Bus::Session).is_empty(),
        "no retry by itself"
    );

    // The daemon starts, so the handle subscribes.
    source.deliver(Bus::Session, owner(""));
    source.deliver(Bus::Session, owner(DAEMON));
    assert_eq!(members(&source.take_calls(Bus::Session)), ["Subscribe"]);
}

/// A refusal is an answer, so it never falls back to defaults.
#[test]
fn a_refused_shared_open_fails() {
    let tree = Tree::new();
    tree.install(DESKTOP);
    let source = tree.source();
    reach(&source);
    let ticket = source.request_namespace(DESKTOP_NS);
    let calls = source.take_calls(Bus::Session);
    source.deliver(Bus::Session, failed(calls[0].id, ACCESS_DENIED));
    let e = common::opened(&source, ticket).unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Denied, "{e}");
}

/// A daemon serving another version leaves a shared open on its defaults, saying so.
#[test]
fn a_shared_open_at_another_version_stays_on_defaults() {
    let tree = Tree::new();
    tree.install(DESKTOP);
    let source = tree.source();
    // The client was built with v1.
    source.embed(DESKTOP_NS, DESKTOP);
    reach(&source);
    let ticket = source.request_namespace(DESKTOP_NS);
    let calls = source.take_calls(Bus::Session);
    let newer = DESKTOP.replace("version = 1", "version = 2");
    let reply = subscribed(&newer, 1, &BTreeMap::new(), &[]);
    source.deliver(Bus::Session, answer(calls[0].id, reply));
    let settings = common::opened(&source, ticket).unwrap();
    assert!(settings.on_defaults());
    let diagnostics = settings.diagnostics();
    let [conflict] = &*diagnostics else {
        panic!("{diagnostics:?}")
    };
    assert!(
        conflict.is_error() && conflict.message().contains("v2"),
        "{conflict:?}"
    );
}

/// A daemon that starts later at another version leaves the defaults handle read-only.
#[test]
fn a_daemon_starting_at_another_version_leaves_the_defaults_read_only() {
    let tree = Tree::new();
    tree.install(DESKTOP);
    let source = tree.source();
    source.embed(DESKTOP_NS, DESKTOP);
    let settings = open(&source, DESKTOP_NS).unwrap();
    let calls = reach_bus(&source, Bus::Session);
    let newer = DESKTOP.replace("version = 1", "version = 2");
    let args = subscribed(&newer, 1, &BTreeMap::new(), &[]);
    source.deliver(Bus::Session, answer(calls[0].id, args));
    assert!(settings.on_defaults());
    assert_eq!(settings.other_version(), Some(2));
    let refused = settings
        .write(&[Edit::set("accent", text("red"))])
        .unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::VersionConflict, "{refused}");
    assert!(
        source.take_calls(Bus::Session).is_empty(),
        "nothing is sent"
    );
}
