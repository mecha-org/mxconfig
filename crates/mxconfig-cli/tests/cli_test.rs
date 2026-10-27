//! Tests that drive the real binary.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

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
"##;

fn run(args: &[&str], envs: &[(&str, &Path)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mxconfig"))
        .args(args)
        .envs(envs.iter().copied())
        .output()
        .expect("the command runs")
}

#[test]
fn check_accepts_a_valid_schema_and_rejects_a_broken_one() {
    let dir = TempDir::new().unwrap();
    let good = dir.path().join("good.toml");
    fs::write(&good, SCHEMA).unwrap();

    let output = run(&["check", good.to_str().unwrap()], &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("ok      org.mechanix.demo"), "{stdout}");

    let broken = dir.path().join("broken.toml");
    fs::write(&broken, "namespace = \"org.mechanix.broken\"\nversion = 1\n\n[keys.k]\ntype = \"integer\"\nlabel = \"K\"\n").unwrap();
    let output = run(&["check", broken.to_str().unwrap()], &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!output.status.success(), "{stdout}");
    assert!(stdout.contains("no description"), "{stdout}");
}

#[test]
fn dump_describes_values_and_prune_drops_stale_keys() {
    let tree = Layers::with("org.mechanix.demo", SCHEMA);
    let user_file = tree.put(
        "user",
        "org.mechanix.demo/config.toml",
        "[colors]\naccent = \"#ff0000\"\nbogus = 1\n",
    );

    let (ok, out) = tree.run(&["dump", "org.mechanix.demo"], &[]);
    assert!(ok, "{out}");
    assert!(
        out.contains("org.mechanix.demo — app class, schema v1"),
        "{out}"
    );
    assert!(out.contains("colors.accent = \"#ff0000\""), "{out}");
    let unknown = format!(
        "warning: {}: unknown key `colors.bogus`",
        user_file.display()
    );
    assert!(out.contains(&unknown), "{out}");

    let (ok, out) = tree.run(&["prune", "org.mechanix.demo"], &[]);
    assert!(ok, "{out}");
    assert!(out.contains("removed colors.bogus"), "{out}");
    let text = fs::read_to_string(&user_file).unwrap();
    assert!(!text.contains("bogus"), "{text}");
    assert!(text.contains("#ff0000"), "{text}");
}

#[test]
fn assets_lists_declared_assets_and_origins() {
    const ASSET_SCHEMA: &str = r#"
namespace = "org.mechanix.demo"
version = 1

[assets.layout]
path = "ui/layout.yaml"

[keys.k]
type = "integer"
default = 1
label = "K"
description = "d"
"#;
    let tree = Layers::with("org.mechanix.demo", ASSET_SCHEMA);
    tree.put("user", "org.mechanix.demo/ui/layout.yaml", "hello");

    let (ok, out) = tree.run(&["assets", "org.mechanix.demo"], &[]);
    assert!(ok, "{out}");
    assert!(out.contains("layout = ui/layout.yaml (5 bytes)"), "{out}");
    assert!(out.contains("[user"), "{out}");
}

#[test]
fn dump_shows_a_key_overridden_from_env() {
    const ENV_SCHEMA: &str = r#"
namespace = "org.mechanix.demo"
version = 1

[keys.scale]
type = "float"
default = 1.0
env = "MXCONFIG_TEST_DEMO_SCALE"
label = "Scale"
description = "d"
"#;
    let tree = Layers::with("org.mechanix.demo", ENV_SCHEMA);
    let scale = [("MXCONFIG_TEST_DEMO_SCALE", Path::new("1.5"))];

    let (ok, out) = tree.run(&["dump", "org.mechanix.demo"], &scale);
    assert!(ok, "{out}");
    assert!(out.contains("scale = 1.5"), "{out}");
    assert!(out.contains("env $MXCONFIG_TEST_DEMO_SCALE"), "{out}");
}

#[test]
fn check_fails_a_manifest_with_a_temporary_grant() {
    let dir = TempDir::new().unwrap();
    let manifest = dir.path().join("org.example.player.toml");
    let lasting = "client = \"org.example.player\"\n\n[[settings]]\nnamespace = \"org.mechanix.desktop.theme\"\naccess = \"read\"\n";
    fs::write(&manifest, lasting).unwrap();
    let output = run(&["check", manifest.to_str().unwrap()], &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(
        stdout.contains("ok      org.example.player manifest"),
        "{stdout}"
    );

    let temporary = format!(
        "{lasting}\n[[settings]]\nnamespace = \"org.mechanix.desktop.theme\"\naccess = \"read-write\"\ntemporary = true\n"
    );
    fs::write(&manifest, temporary).unwrap();
    let output = run(&["check", manifest.to_str().unwrap()], &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!output.status.success(), "{stdout}");
    assert!(stdout.contains("must not ship"), "{stdout}");
}

#[test]
fn flatpak_asks_for_an_installed_flatpak_app() {
    let output = run(&["dump", "--flatpak", "org.mechanix.nothing"], &[]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("not an installed Flatpak app"), "{stderr}");
}

/// A schema with a managed key and asset beside unmanaged ones.
const MANAGED: &str = r#"
namespace = "org.mechanix.demo"
version = 1

[assets.layout]
path = "layout.yaml"
managed = true
env = "MXCONFIG_TEST_MANAGED_LAYOUT"

[assets.theme]
path = "theme.css"
env = "MXCONFIG_TEST_MANAGED_THEME"

[keys.brightness]
type = "integer"
default = 50
label = "Brightness"
description = "Panel brightness."
managed = true
env = "MXCONFIG_TEST_MANAGED_BRIGHTNESS"

[keys.volume]
type = "integer"
default = 50
label = "Volume"
description = "Output volume."
env = "MXCONFIG_TEST_MANAGED_VOLUME"
"#;

/// A tree of schema and layer dirs, each set through its env var.
struct Layers {
    root: TempDir,
}

impl Layers {
    /// A tree with `schema` installed for `namespace`.
    fn with(namespace: &str, schema: &str) -> Layers {
        let root = TempDir::new().unwrap();
        for dir in ["schemas", "vendor", "admin", "user"] {
            fs::create_dir_all(root.path().join(dir)).unwrap();
        }
        let file = root
            .path()
            .join("schemas")
            .join(format!("{namespace}.toml"));
        fs::write(file, schema).unwrap();
        Layers { root }
    }

    fn dir(&self, layer: &str) -> std::path::PathBuf {
        self.root.path().join(layer)
    }

    /// Write `text` at `path` under `layer`, creating its parents.
    fn put(&self, layer: &str, path: &str, text: &str) -> std::path::PathBuf {
        let file = self.dir(layer).join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, text).unwrap();
        file
    }

    /// Run the CLI over this tree, with `extra` env vars.
    fn run(&self, args: &[&str], extra: &[(&str, &Path)]) -> (bool, String) {
        let (schemas, vendor) = (self.dir("schemas"), self.dir("vendor"));
        let (admin, user) = (self.dir("admin"), self.dir("user"));
        let mut envs = vec![
            ("MXCONFIG_SCHEMA_DIR", schemas.as_path()),
            ("MXCONFIG_VENDOR_DIR", vendor.as_path()),
            ("MXCONFIG_ADMIN_DIR", admin.as_path()),
            ("MXCONFIG_USER_DIR", user.as_path()),
        ];
        envs.extend_from_slice(extra);
        let output = run(args, &envs);
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr);
        (output.status.success(), format!("{stdout}{stderr}"))
    }
}

/// `describe` marks a managed key, and only that key.
#[test]
fn describe_marks_managed_keys() {
    let tree = Layers::with("org.mechanix.demo", MANAGED);
    let (ok, out) = tree.run(&["describe", "org.mechanix.demo"], &[]);
    assert!(ok, "{out}");
    assert!(
        out.contains("type integer, managed, default 50, env $MXCONFIG_TEST_MANAGED_BRIGHTNESS"),
        "{out}"
    );
    assert!(
        out.contains("type integer, default 50, env $MXCONFIG_TEST_MANAGED_VOLUME"),
        "{out}"
    );
    assert_eq!(out.matches(", managed").count(), 1, "{out}");
}

/// `dump` shows the admin value of a managed key over the user's.
#[test]
fn dump_shows_the_admin_value_for_a_managed_key() {
    let tree = Layers::with("org.mechanix.demo", MANAGED);
    let admin = tree.put(
        "admin",
        "org.mechanix.demo/config.toml",
        "brightness = 20\nvolume = 30\n",
    );
    let user = tree.put(
        "user",
        "org.mechanix.demo/config.toml",
        "brightness = 99\nvolume = 70\n",
    );
    let (ok, out) = tree.run(&["dump", "org.mechanix.demo"], &[]);
    assert!(ok, "{out}");
    let admin = admin.display();
    assert!(
        out.contains(&format!("brightness = 20    [admin {admin}]")),
        "{out}"
    );
    assert!(out.contains("volume = 70    [user "), "{out}");
    let ignored = format!(
        "warning: {}: `brightness` is managed, so the user's value is ignored",
        user.display()
    );
    assert!(out.contains(&ignored), "{out}");
    assert!(
        out.contains(&format!("warning: {admin} is writable by the current user")),
        "{out}"
    );
}

/// `dump` never reads a managed key's env var, but reads an unmanaged one's.
#[test]
fn dump_ignores_env_for_a_managed_key() {
    let tree = Layers::with("org.mechanix.demo", MANAGED);
    tree.put(
        "vendor",
        "org.mechanix.demo/config.toml",
        "brightness = 10\n",
    );
    let extra = [
        ("MXCONFIG_TEST_MANAGED_BRIGHTNESS", Path::new("77")),
        ("MXCONFIG_TEST_MANAGED_VOLUME", Path::new("66")),
    ];
    let (ok, out) = tree.run(&["dump", "org.mechanix.demo"], &extra);
    assert!(ok, "{out}");
    assert!(out.contains("brightness = 10    [vendor "), "{out}");
    assert!(!out.contains("brightness = 77"), "{out}");
    assert!(
        out.contains("volume = 66    [env $MXCONFIG_TEST_MANAGED_VOLUME]"),
        "{out}"
    );
}

/// A managed key with no layer value dumps its schema default.
#[test]
fn dump_shows_the_default_for_an_unset_managed_key() {
    let tree = Layers::with("org.mechanix.demo", MANAGED);
    tree.put("user", "org.mechanix.demo/config.toml", "brightness = 99\n");
    let (ok, out) = tree.run(&["dump", "org.mechanix.demo"], &[]);
    assert!(ok, "{out}");
    assert!(out.contains("brightness = 50    [schema]"), "{out}");
}

/// `assets` skips a managed asset's user copy and env var.
#[test]
fn assets_ignores_the_user_copy_and_env_for_a_managed_asset() {
    let tree = Layers::with("org.mechanix.demo", MANAGED);
    tree.put("user", "org.mechanix.demo/layout.yaml", "user");
    tree.put("user", "org.mechanix.demo/theme.css", "user");
    let outside = TempDir::new().unwrap();
    let elsewhere = outside.path().join("elsewhere");
    fs::write(&elsewhere, "from env").unwrap();
    let extra = [
        ("MXCONFIG_TEST_MANAGED_LAYOUT", elsewhere.as_path()),
        ("MXCONFIG_TEST_MANAGED_THEME", elsewhere.as_path()),
    ];
    let (ok, out) = tree.run(&["assets", "org.mechanix.demo"], &extra);
    assert!(ok, "{out}");
    assert!(out.contains("layout = layout.yaml    [unset]"), "{out}");
    let theme = format!("theme = {} (8 bytes)", elsewhere.display());
    assert!(out.contains(&theme), "{out}");

    tree.put("admin", "org.mechanix.demo/layout.yaml", "admin");
    let (ok, out) = tree.run(&["assets", "org.mechanix.demo"], &extra);
    assert!(ok, "{out}");
    assert!(
        out.contains("layout = layout.yaml (5 bytes)    [admin "),
        "{out}"
    );
}

/// `prune` drops unknown keys but keeps the ignored managed value.
#[test]
fn prune_keeps_a_managed_key_and_dump_still_ignores_it() {
    let tree = Layers::with("org.mechanix.demo", MANAGED);
    tree.put(
        "admin",
        "org.mechanix.demo/config.toml",
        "brightness = 20\n",
    );
    let user = tree.put(
        "user",
        "org.mechanix.demo/config.toml",
        "brightness = 99\nbogus = 1\n",
    );
    let (ok, out) = tree.run(&["prune", "org.mechanix.demo"], &[]);
    assert!(ok, "{out}");
    assert!(out.contains("removed bogus"), "{out}");
    assert!(!out.contains("removed brightness"), "{out}");
    let text = fs::read_to_string(&user).unwrap();
    assert!(text.contains("brightness = 99"), "{text}");
    assert!(!text.contains("bogus"), "{text}");

    let (ok, out) = tree.run(&["dump", "org.mechanix.demo"], &[]);
    assert!(ok, "{out}");
    assert!(out.contains("brightness = 20    [admin "), "{out}");
}

/// `check` fails a schema that declares env on a managed key or asset.
#[test]
fn check_flags_env_on_a_managed_key_or_asset() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("org.mechanix.demo.toml");
    fs::write(&file, MANAGED).unwrap();
    let output = run(&["check", file.to_str().unwrap()], &[]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!output.status.success(), "{stdout}");
    assert!(
        stdout.contains("`brightness` is managed, so its env var is never read"),
        "{stdout}"
    );
    assert!(
        stdout.contains("`layout` is managed, so its env var is never read"),
        "{stdout}"
    );
    assert!(!stdout.contains("`volume` is managed"), "{stdout}");
}

/// `dump` of a privileged namespace reads system layers, never the user's.
#[test]
fn dump_of_a_privileged_namespace_ignores_the_user_dir() {
    const PRIVILEGED: &str = r#"
namespace = "org.mechanix.privileged.display"
version = 1

[keys.brightness]
type = "integer"
default = 50
label = "Brightness"
description = "Panel brightness."
"#;
    let namespace = "org.mechanix.privileged.display";
    let tree = Layers::with(namespace, PRIVILEGED);
    let home = format!("mxconfig/{namespace}/conf.toml");
    tree.put("admin", &home, "brightness = 15\n");
    tree.put("user", &home, "brightness = 99\n");
    let (ok, out) = tree.run(&["dump", namespace], &[]);
    assert!(ok, "{out}");
    assert!(out.contains("privileged class"), "{out}");
    assert!(out.contains("brightness = 15    [admin "), "{out}");
    assert!(!out.contains("brightness = 99"), "{out}");
    let (ok, out) = tree.run(&["prune", namespace], &[]);
    assert!(
        !ok && out.contains("only the privileged service writes it"),
        "{out}"
    );
}

/// `set` and `unset` write the user's file, checked against the schema.
#[test]
fn set_and_unset_write_the_user_file() {
    const LOOK: &str = r##"
namespace = "org.mechanix.desktop.look"
version = 1

[keys.accent]
type = "string"
default = "#7aa2f7"
label = "Accent"
description = "The accent color."

[keys.level]
type = "integer"
default = 50
min = 0
max = 100
label = "Level"
description = "A level."

[keys.scale]
type = "float"
default = 1.0
label = "Scale"
description = "A scale."
"##;
    let namespace = "org.mechanix.desktop.look";
    let tree = Layers::with(namespace, LOOK);
    let file = tree
        .dir("user")
        .join(format!("mxconfig/{namespace}/conf.toml"));

    let (ok, out) = tree.run(&["set", namespace, "level", "60"], &[]);
    assert!(ok, "{out}");
    // A string key takes plain text.
    let (ok, out) = tree.run(&["set", namespace, "accent", "#ff0000"], &[]);
    assert!(ok, "{out}");
    // A float key takes a whole number.
    let (ok, out) = tree.run(&["set", namespace, "scale", "2"], &[]);
    assert!(ok, "{out}");
    let written = fs::read_to_string(&file).unwrap();
    assert!(written.contains("level = 60"), "{written}");
    assert!(written.contains("accent = \"#ff0000\""), "{written}");
    assert!(written.contains("scale = 2.0"), "{written}");

    // Bad values are refused, and change nothing.
    let (ok, out) = tree.run(&["set", namespace, "level", "high"], &[]);
    assert!(!ok && out.contains("not a TOML value"), "{out}");
    let (ok, out) = tree.run(&["set", namespace, "level", "500"], &[]);
    assert!(!ok && out.contains("level"), "{out}");
    let (ok, out) = tree.run(&["set", namespace, "nothing", "1"], &[]);
    assert!(!ok && out.contains("nothing"), "{out}");
    assert_eq!(fs::read_to_string(&file).unwrap(), written);

    let (ok, out) = tree.run(&["unset", namespace, "level"], &[]);
    assert!(ok, "{out}");
    let unset = fs::read_to_string(&file).unwrap();
    assert!(
        !unset.contains("level") && unset.contains("accent"),
        "{unset}"
    );
}

/// `set` leaves a privileged namespace to the privileged service.
#[test]
fn set_refuses_a_privileged_namespace() {
    let namespace = "org.mechanix.privileged.panel";
    let schema = format!(
        "namespace = \"{namespace}\"\nversion = 1\n\n[keys.level]\ntype = \"integer\"\ndefault = 1\nlabel = \"L\"\ndescription = \"d\"\n"
    );
    let tree = Layers::with(namespace, &schema);
    let (ok, out) = tree.run(&["set", namespace, "level", "2"], &[]);
    assert!(
        !ok && out.contains("only the privileged service writes it"),
        "{out}"
    );
}

/// A relative override is an error, not a silent fallback to the real files.
#[test]
fn a_relative_override_is_refused() {
    let tree = Layers::with("org.mechanix.demo", SCHEMA);
    let relative = [("MXCONFIG_USER_DIR", Path::new("user"))];
    let (ok, out) = tree.run(
        &["set", "org.mechanix.demo", "colors.accent", "red"],
        &relative,
    );
    assert!(
        !ok && out.contains("MXCONFIG_USER_DIR must be an absolute path"),
        "{out}"
    );
}
