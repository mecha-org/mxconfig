//! Migrations of the user file, through the file backend.

mod common;

use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;

use common::{Through, at, detached, edge, int, key, text};
use mxconfig::host::Roots;
use mxconfig::runtime::Drive;
use mxconfig::{Bus, Error, ErrorKind, Settings, Source, Value};

/// The app namespace the tests migrate.
const NS: &str = "org.example.app";

/// A group declaration.
fn group(id: &str) -> String {
    format!("[groups.{id}]\nlabel = \"G\"\n")
}

/// A temporary tree of roots for the file backend.
struct Tree {
    _tmp: tempfile::TempDir,
    roots: Roots,
}

impl Tree {
    fn new() -> Tree {
        let tmp = tempfile::tempdir().unwrap();
        let roots = Roots::under(tmp.path());
        fs::create_dir_all(&roots.schema_dirs[0]).unwrap();
        fs::create_dir_all(Self::dir_in(&roots)).unwrap();
        Tree { _tmp: tmp, roots }
    }

    fn dir_in(roots: &Roots) -> PathBuf {
        roots.user_dir.as_ref().unwrap().join(NS)
    }

    /// The namespace's user directory.
    fn dir(&self) -> PathBuf {
        Self::dir_in(&self.roots)
    }

    fn file(&self) -> PathBuf {
        self.dir().join("config.toml")
    }

    fn state(&self) -> PathBuf {
        self.dir().join("config.toml.state")
    }

    /// Install `schema` as the namespace's schema file.
    fn install(&self, schema: &str) -> PathBuf {
        common::install(&self.roots, NS, schema)
    }

    /// Install `schema`, then open the namespace on a new source.
    fn open(&self, schema: &str) -> Result<Settings, Error> {
        self.install(schema);
        common::open(&Source::with_roots(self.roots.clone()), NS)
    }

    /// Write the user file as version `version` wrote it.
    fn seed(&self, version: u32, text: &str) {
        fs::write(self.file(), text).unwrap();
        fs::write(self.state(), format!("version = {version}\n")).unwrap();
    }

    fn text(&self) -> String {
        fs::read_to_string(self.file()).unwrap()
    }

    /// The file's parsed contents.
    fn table(&self) -> toml::Table {
        self.text().parse().unwrap()
    }

    /// The recorded version and the edges kept in `.state`.
    fn recorded(&self) -> (i64, Vec<(i64, i64)>) {
        common::recorded(&self.file()).expect("a state file")
    }

    /// Every file in the user directory, with its bytes.
    fn snapshot(&self) -> Vec<(PathBuf, Vec<u8>)> {
        let mut files: Vec<_> = walk(&self.dir())
            .into_iter()
            .map(|path| {
                let bytes = fs::read(&path).unwrap();
                (path, bytes)
            })
            .collect();
        files.sort();
        files
    }
}

/// Every file below `dir`.
fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(walk(&path));
        } else {
            found.push(path);
        }
    }
    found
}

/// Whether a diagnostic that is not an error mentions `needle`.
fn warned(settings: &Settings, needle: &str) -> bool {
    let diags = settings.diagnostics();
    diags
        .iter()
        .any(|d| !d.is_error() && d.message().contains(needle))
}

/// The error diagnostics on `settings`.
fn errors(settings: &Settings) -> Vec<String> {
    let diags = settings
        .diagnostics()
        .into_iter()
        .filter(mxconfig::Diagnostic::is_error);
    diags.map(|d| d.message().to_string()).collect()
}

// Each op, upward.

/// A rename moves a top-level value to its new key.
#[test]
fn a_rename_moves_a_top_level_value() {
    let tree = Tree::new();
    tree.seed(1, "old = 5\n");
    let v2 = at(
        NS,
        2,
        &(key("new", "integer") + &edge(1, 2, &[r#"{ op = "rename", from = "old", to = "new" }"#])),
    );
    let settings = tree.open(&v2).unwrap();
    assert_eq!(settings.get("new"), Some(int(5)));
    let table = tree.table();
    assert!(!table.contains_key("old"), "{table:?}");
    assert_eq!(table["new"].as_integer(), Some(5));
}

/// A rename moves a top-level value into a group.
#[test]
fn a_rename_moves_a_value_into_a_group() {
    let tree = Tree::new();
    tree.seed(1, "radius = 3\nother = true\n");
    let body = group("colors")
        + &key("colors.radius", "integer")
        + &key("other", "bool")
        + &edge(
            1,
            2,
            &[r#"{ op = "rename", from = "radius", to = "colors.radius" }"#],
        );
    let settings = tree.open(&at(NS, 2, &body)).unwrap();
    assert_eq!(settings.get("colors.radius"), Some(int(3)));
    assert_eq!(settings.get("other"), Some(Value::Boolean(true)));
    assert!(!tree.table().contains_key("radius"), "{}", tree.text());
}

/// A rename moves a grouped value out to the top level.
#[test]
fn a_rename_moves_a_value_out_of_a_group() {
    let tree = Tree::new();
    tree.seed(1, "[colors]\nradius = 3\naccent = \"red\"\n");
    let body = group("colors")
        + &key("colors.accent", "string")
        + &key("radius", "integer")
        + &edge(
            1,
            2,
            &[r#"{ op = "rename", from = "colors.radius", to = "radius" }"#],
        );
    let settings = tree.open(&at(NS, 2, &body)).unwrap();
    assert_eq!(settings.get("radius"), Some(int(3)));
    assert_eq!(settings.get("colors.accent"), Some(text("red")));
    let table = tree.table();
    assert!(!table["colors"].as_table().unwrap().contains_key("radius"));
}

/// A rename reaches values nested deeper than a group, either way.
#[test]
fn a_rename_follows_nested_paths() {
    let tree = Tree::new();
    tree.seed(1, "[legacy.ui.panel]\nsize = 4\n[deep.a]\nb = 1\n");
    let body = group("ui")
        + &key("ui.size", "integer")
        + &key("ui.extra", "map")
        + &edge(
            1,
            2,
            &[
                r#"{ op = "rename", from = "legacy.ui.panel.size", to = "ui.size" }"#,
                r#"{ op = "rename", from = "deep.a.b", to = "ui.extra.inner" }"#,
            ],
        );
    let settings = tree.open(&at(NS, 2, &body)).unwrap();
    assert_eq!(settings.get("ui.size"), Some(int(4)));
    let table = tree.table();
    assert_eq!(table["ui"]["extra"]["inner"].as_integer(), Some(1));
    assert!(
        table["legacy"]["ui"]["panel"]
            .as_table()
            .unwrap()
            .is_empty()
    );
}

/// A rename reads a value written as a dotted key.
#[test]
fn a_rename_reads_a_dotted_key() {
    let tree = Tree::new();
    tree.seed(1, "colors.radius = 6\n");
    let body = group("colors")
        + &key("colors.corner", "integer")
        + &edge(
            1,
            2,
            &[r#"{ op = "rename", from = "colors.radius", to = "colors.corner" }"#],
        );
    let settings = tree.open(&at(NS, 2, &body)).unwrap();
    assert_eq!(settings.get("colors.corner"), Some(int(6)));
}

/// A rename into a group written as an inline table keeps its other values.
#[test]
fn a_rename_into_an_inline_table_keeps_its_siblings() {
    let tree = Tree::new();
    tree.seed(1, "radius = 3\ncolors = { accent = \"red\" }\n");
    let body = group("colors")
        + &key("colors.accent", "string")
        + &key("colors.radius", "integer")
        + &edge(
            1,
            2,
            &[r#"{ op = "rename", from = "radius", to = "colors.radius" }"#],
        );
    let settings = tree.open(&at(NS, 2, &body)).unwrap();
    assert_eq!(settings.get("colors.radius"), Some(int(3)));
    assert_eq!(
        settings.get("colors.accent"),
        Some(text("red")),
        "{}",
        tree.text()
    );
}

/// A retype converts a value set in an inline table.
#[test]
fn a_retype_converts_a_value_in_an_inline_table() {
    let tree = Tree::new();
    tree.seed(1, "ui = { count = 4 }\n");
    let body = group("ui")
        + &key("ui.count", "string")
        + &edge(
            1,
            2,
            &[r#"{ op = "retype", key = "ui.count", from = "integer", to = "string" }"#],
        );
    let settings = tree.open(&at(NS, 2, &body)).unwrap();
    assert_eq!(
        settings.get("ui.count"),
        Some(text("4")),
        "{:?}",
        settings.diagnostics()
    );
}

/// A write into a group set as an inline table keeps it inline.
#[test]
fn a_write_into_an_inline_table_keeps_its_siblings() {
    let tree = Tree::new();
    tree.seed(1, "colors = { accent = \"red\", mode = \"dark\" }\n");
    let body = group("colors") + &key("colors.accent", "string") + &key("colors.mode", "string");
    let settings = tree.open(&at(NS, 1, &body)).unwrap();
    settings.set("colors.mode", text("light")).unwrap();
    let want = "colors = { accent = \"red\", mode = \"light\" }\n";
    assert_eq!(tree.text(), want);
    assert_eq!(settings.get("colors.accent"), Some(text("red")));
}

/// A rename of a key the file never set warns and still opens.
#[test]
fn a_rename_of_an_absent_key_warns() {
    let tree = Tree::new();
    tree.seed(1, "other = 1\n");
    let body = key("new", "integer")
        + &key("other", "integer")
        + &edge(1, 2, &[r#"{ op = "rename", from = "old", to = "new" }"#]);
    let settings = tree.open(&at(NS, 2, &body)).unwrap();
    assert_eq!(settings.get("new"), None);
    assert!(
        warned(&settings, "`old` is not present"),
        "{:?}",
        settings.diagnostics()
    );
    assert_eq!(tree.recorded().0, 2);
}

/// A retype converts between integers and strings, both ways.
#[test]
fn a_retype_converts_between_integers_and_strings() {
    let tree = Tree::new();
    tree.seed(1, "count = 42\nlimit = \"17\"\n");
    let body = key("count", "string")
        + &key("limit", "integer")
        + &edge(
            1,
            2,
            &[
                r#"{ op = "retype", key = "count", from = "integer", to = "string" }"#,
                r#"{ op = "retype", key = "limit", from = "string", to = "integer" }"#,
            ],
        );
    let settings = tree.open(&at(NS, 2, &body)).unwrap();
    assert_eq!(settings.get("count"), Some(text("42")));
    assert_eq!(settings.get("limit"), Some(int(17)));
    assert!(errors(&settings).is_empty(), "{:?}", settings.diagnostics());
}

/// A retype converts between bools and strings, both ways.
#[test]
fn a_retype_converts_between_bools_and_strings() {
    let tree = Tree::new();
    tree.seed(1, "on = true\noff = \"false\"\n");
    let body = key("on", "string")
        + &key("off", "bool")
        + &edge(
            1,
            2,
            &[
                r#"{ op = "retype", key = "on", from = "bool", to = "string" }"#,
                r#"{ op = "retype", key = "off", from = "string", to = "bool" }"#,
            ],
        );
    let settings = tree.open(&at(NS, 2, &body)).unwrap();
    assert_eq!(settings.get("on"), Some(text("true")));
    assert_eq!(settings.get("off"), Some(Value::Boolean(false)));
}

/// A retype converts between integers and floats, both ways.
#[test]
fn a_retype_converts_between_integers_and_floats() {
    let tree = Tree::new();
    tree.seed(1, "scale = 2\nratio = 2.75\n");
    let body = key("scale", "float")
        + &key("ratio", "integer")
        + &edge(
            1,
            2,
            &[
                r#"{ op = "retype", key = "scale", from = "integer", to = "float" }"#,
                r#"{ op = "retype", key = "ratio", from = "float", to = "integer" }"#,
            ],
        );
    let settings = tree.open(&at(NS, 2, &body)).unwrap();
    assert_eq!(settings.get("scale"), Some(Value::Float(2.0)));
    assert_eq!(
        settings.get("ratio"),
        Some(int(2)),
        "truncated as `as` does"
    );
}

/// A retype of a key the file never set does nothing.
#[test]
fn a_retype_of_an_absent_key_is_a_no_op() {
    let tree = Tree::new();
    tree.seed(1, "other = 1\n");
    let body = key("count", "string")
        + &key("other", "integer")
        + &edge(
            1,
            2,
            &[r#"{ op = "retype", key = "count", from = "integer", to = "string" }"#],
        );
    let settings = tree.open(&at(NS, 2, &body)).unwrap();
    assert_eq!(settings.get("count"), None);
    assert!(!tree.table().contains_key("count"));
    assert_eq!(tree.recorded().0, 2);
}

/// A value a retype can't convert fails the open and writes nothing.
#[test]
fn an_unconvertible_retype_fails_the_open_and_writes_nothing() {
    let tree = Tree::new();
    tree.seed(1, "count = \"many\"\nflag = 3\n");
    let before = tree.snapshot();
    for op in [
        r#"{ op = "retype", key = "count", from = "string", to = "integer" }"#,
        r#"{ op = "retype", key = "flag", from = "string", to = "bool" }"#,
        r#"{ op = "retype", key = "count", from = "string", to = "datetime" }"#,
    ] {
        let body = key("count", "integer") + &key("flag", "bool") + &edge(1, 2, &[op]);
        let e = tree.open(&at(NS, 2, &body)).unwrap_err().to_string();
        assert!(e.contains("migration failed"), "{e}");
        assert!(e.contains("cannot coerce"), "{e}");
        assert_eq!(tree.snapshot(), before, "nothing was written for {op}");
    }
}

/// A remap replaces a string through its map.
#[test]
fn a_remap_replaces_a_string() {
    let tree = Tree::new();
    tree.seed(1, "theme = \"night\"\n");
    let body = format!(
        "[keys.theme]\ntype = \"enum\"\nvalues = [\"dark\", \"light\"]\nlabel = \"L\"\ndescription = \"d\"\n{}",
        edge(
            1,
            2,
            &[r#"{ op = "remap", key = "theme", map = { night = "dark", day = "light" } }"#]
        )
    );
    let settings = tree.open(&at(NS, 2, &body)).unwrap();
    assert_eq!(settings.get("theme"), Some(text("dark")));
    assert_eq!(tree.table()["theme"].as_str(), Some("dark"));
}

/// A remap of a string outside its map warns and leaves it.
#[test]
fn a_remap_of_an_unmapped_string_warns() {
    let tree = Tree::new();
    tree.seed(1, "theme = \"dusk\"\n");
    let body = key("theme", "string")
        + &edge(
            1,
            2,
            &[r#"{ op = "remap", key = "theme", map = { night = "dark" } }"#],
        );
    let settings = tree.open(&at(NS, 2, &body)).unwrap();
    assert_eq!(settings.get("theme"), Some(text("dusk")));
    assert!(
        warned(&settings, "`dusk` is not in the map"),
        "{:?}",
        settings.diagnostics()
    );
}

/// A rename-asset moves the file, into a new directory too.
#[test]
fn a_rename_asset_moves_the_file() {
    let tree = Tree::new();
    tree.seed(1, "");
    fs::write(tree.dir().join("layout.yaml"), "mine").unwrap();
    let body = "[assets.layout]\npath = \"ui/layout.kdl\"\n".to_string()
        + &edge(
            1,
            2,
            &[r#"{ op = "rename-asset", from = "layout.yaml", to = "ui/layout.kdl" }"#],
        );
    let settings = tree.open(&at(NS, 2, &body)).unwrap();
    assert!(!tree.dir().join("layout.yaml").exists());
    let moved = tree.dir().join("ui/layout.kdl");
    assert_eq!(fs::read_to_string(&moved).unwrap(), "mine");
    let asset = settings.asset("layout").unwrap();
    assert_eq!(asset.path, moved);
    assert_eq!(asset.text().unwrap(), "mine");
}

/// A rename-asset of a file that is not there warns.
#[test]
fn a_rename_asset_of_a_missing_file_warns() {
    let tree = Tree::new();
    tree.seed(1, "");
    let body = "[assets.layout]\npath = \"layout.kdl\"\n".to_string()
        + &edge(
            1,
            2,
            &[r#"{ op = "rename-asset", from = "layout.yaml", to = "layout.kdl" }"#],
        );
    let settings = tree.open(&at(NS, 2, &body)).unwrap();
    assert!(settings.asset("layout").is_none());
    assert!(warned(&settings, "asset `layout.yaml` is not present"));
}

// Chains and the state file.

/// A v1 → v2 → v3 schema: rename, then retype.
fn chain() -> String {
    let body = key("size", "string")
        + &edge(1, 2, &[r#"{ op = "rename", from = "width", to = "size" }"#])
        + &edge(
            2,
            3,
            &[r#"{ op = "retype", key = "size", from = "integer", to = "string" }"#],
        );
    at(NS, 3, &body)
}

/// One open crosses every edge and records each one.
#[test]
fn one_open_crosses_every_edge_and_records_them() {
    let tree = Tree::new();
    tree.seed(1, "width = 12\n");
    let settings = tree.open(&chain()).unwrap();
    assert_eq!(settings.get("size"), Some(text("12")));
    assert_eq!(tree.recorded(), (3, vec![(1, 2), (2, 3)]));
    let table = tree.table();
    assert!(!table.contains_key("width"), "{table:?}");
    assert!(!tree.text().contains("__"), "no library keys: {table:?}");
}

/// A file part way along the chain crosses only the edges it needs.
#[test]
fn a_file_part_way_crosses_only_the_rest() {
    let tree = Tree::new();
    tree.seed(2, "size = 7\nwidth = 1\n");
    let settings = tree.open(&chain()).unwrap();
    assert_eq!(settings.get("size"), Some(text("7")));
    assert_eq!(tree.recorded(), (3, vec![(2, 3)]));
    assert_eq!(
        tree.table()["width"].as_integer(),
        Some(1),
        "not renamed again"
    );
}

/// The state file keeps only the newest three edges.
#[test]
fn the_state_keeps_at_most_three_edges() {
    let tree = Tree::new();
    tree.seed(1, "k1 = 1\n");
    let mut body = key("k5", "integer");
    for v in 1..5 {
        let op = format!(r#"{{ op = "rename", from = "k{v}", to = "k{}" }}"#, v + 1);
        body += &edge(v, v + 1, &[&op]);
    }
    let settings = tree.open(&at(NS, 5, &body)).unwrap();
    assert_eq!(settings.get("k5"), Some(int(1)));
    assert_eq!(tree.recorded(), (5, vec![(2, 3), (3, 4), (4, 5)]));
}

/// A later failing edge undoes nothing on disk, even earlier edges.
#[test]
fn a_failing_edge_late_in_a_chain_writes_nothing() {
    let tree = Tree::new();
    tree.seed(1, "width = \"wide\"\n");
    let body = key("size", "integer")
        + &edge(1, 2, &[r#"{ op = "rename", from = "width", to = "size" }"#])
        + &edge(
            2,
            3,
            &[r#"{ op = "retype", key = "size", from = "string", to = "integer" }"#],
        );
    let before = tree.snapshot();
    let e = tree.open(&at(NS, 3, &body)).unwrap_err();
    assert!(e.to_string().contains("cannot coerce `size`"), "{e}");
    assert_eq!(tree.snapshot(), before);
}

/// A failed open through a ticket reports the error as its outcome.
#[test]
fn a_failed_migration_is_the_open_outcome() {
    let tree = Tree::new();
    tree.seed(1, "count = \"many\"\n");
    let body = key("count", "integer")
        + &edge(
            1,
            2,
            &[r#"{ op = "retype", key = "count", from = "string", to = "integer" }"#],
        );
    tree.install(&at(NS, 2, &body));
    let source = Source::with_roots(tree.roots.clone());
    let ticket = source.request_namespace(NS);
    let outcomes = source.outcomes();
    let result = outcomes.iter().find_map(|o| ticket.opened(o)).unwrap();
    let e = result.unwrap_err().to_string();
    assert!(e.contains("migration failed"), "{e}");
}

/// A failed migration leaves an asset an earlier op moved where it was.
#[test]
fn a_failed_migration_leaves_assets_where_they_were() {
    let tree = Tree::new();
    tree.seed(1, "count = \"many\"\n");
    fs::write(tree.dir().join("layout.yaml"), "mine").unwrap();
    let body = key("count", "integer")
        + "[assets.layout]\npath = \"layout.kdl\"\n"
        + &edge(
            1,
            2,
            &[
                r#"{ op = "rename-asset", from = "layout.yaml", to = "layout.kdl" }"#,
                r#"{ op = "retype", key = "count", from = "string", to = "integer" }"#,
            ],
        );
    let before = tree.snapshot();
    assert!(tree.open(&at(NS, 2, &body)).is_err());
    assert_eq!(tree.snapshot(), before);
}

/// A failed migration fails again on retry, and a fixed file then migrates.
#[test]
fn a_fixed_file_migrates_after_a_failure() {
    let tree = Tree::new();
    tree.seed(1, "count = \"many\"\n");
    let body = key("count", "integer")
        + &edge(
            1,
            2,
            &[r#"{ op = "retype", key = "count", from = "string", to = "integer" }"#],
        );
    let schema = at(NS, 2, &body);
    assert!(tree.open(&schema).is_err());
    assert!(
        tree.open(&schema).is_err(),
        "still v1, so it is tried again"
    );
    fs::write(tree.file(), "count = \"3\"\n").unwrap();
    let settings = tree.open(&schema).unwrap();
    assert_eq!(settings.get("count"), Some(int(3)));
    assert_eq!(tree.recorded().0, 2);
}

// Downgrades.

/// A v1 schema without edges, an older build's.
fn old_v1() -> String {
    let body = group("ui")
        + &key("ui.width", "integer")
        + &key("theme", "string")
        + &key("mode", "string");
    at(NS, 1, &(body + "[assets.layout]\npath = \"layout.yaml\"\n"))
}

/// A v3 schema whose edges can all be undone.
fn undoable_v3() -> String {
    let body = group("ui")
        + &key("ui.size", "string")
        + &key("theme", "string")
        + &key("mode", "string")
        + "[assets.layout]\npath = \"ui/layout.kdl\"\n"
        + &edge(
            1,
            2,
            &[
                r#"{ op = "rename", from = "ui.width", to = "ui.size" }"#,
                r#"{ op = "remap", key = "theme", map = { night = "dark" } }"#,
            ],
        )
        + &edge(
            2,
            3,
            &[
                r#"{ op = "retype", key = "ui.size", from = "integer", to = "string" }"#,
                r#"{ op = "rename-asset", from = "layout.yaml", to = "ui/layout.kdl" }"#,
            ],
        );
    at(NS, 3, &body)
}

/// A downgrade replays the recorded edges in reverse.
#[test]
fn a_downgrade_replays_recorded_edges_in_reverse() {
    let tree = Tree::new();
    tree.seed(1, "theme = \"night\"\nmode = \"x\"\n[ui]\nwidth = 640\n");
    fs::write(tree.dir().join("layout.yaml"), "mine").unwrap();
    let up = tree.open(&undoable_v3()).unwrap();
    assert_eq!(up.get("ui.size"), Some(text("640")));
    assert_eq!(up.get("theme"), Some(text("dark")));
    drop(up);

    let down = tree.open(&old_v1()).unwrap();
    assert_eq!(down.get("ui.width"), Some(int(640)));
    assert_eq!(down.get("theme"), Some(text("night")));
    assert_eq!(down.get("mode"), Some(text("x")));
    assert_eq!(down.asset("layout").unwrap().text().unwrap(), "mine");
    assert!(!tree.dir().join("ui/layout.kdl").exists());
    let table = tree.table();
    assert!(!table["ui"].as_table().unwrap().contains_key("size"));
    assert_eq!(tree.recorded(), (1, vec![]));
    assert!(errors(&down).is_empty(), "{:?}", down.diagnostics());
}

/// A downgrade to a middle version undoes only the newer edges.
#[test]
fn a_downgrade_to_a_middle_version_keeps_older_edges() {
    let tree = Tree::new();
    tree.seed(1, "theme = \"night\"\n[ui]\nwidth = 640\n");
    tree.open(&undoable_v3()).unwrap();
    let v2 = group("ui") + &key("ui.size", "integer") + &key("theme", "string");
    let down = tree.open(&at(NS, 2, &v2)).unwrap();
    assert_eq!(down.get("ui.size"), Some(int(640)));
    assert_eq!(tree.recorded(), (2, vec![(1, 2)]));
}

/// A downgrade whose inverse retype can't convert warns, not fails.
#[test]
fn a_downgrade_that_cannot_convert_warns() {
    let tree = Tree::new();
    tree.seed(1, "count = 4\n");
    let v2 = at(
        NS,
        2,
        &(key("count", "string")
            + &edge(
                1,
                2,
                &[r#"{ op = "retype", key = "count", from = "integer", to = "string" }"#],
            )),
    );
    let up = tree.open(&v2).unwrap();
    up.set("count", Value::String("lots".into())).unwrap();
    drop(up);
    let down = tree.open(&at(NS, 1, &key("count", "integer"))).unwrap();
    assert!(
        warned(&down, "cannot coerce `count`"),
        "{:?}",
        down.diagnostics()
    );
    assert_eq!(tree.recorded().0, 1);
}

/// A downgrade with no recorded edge is refused, and leaves the file.
#[test]
fn a_downgrade_without_a_recorded_edge_is_refused() {
    let tree = Tree::new();
    tree.seed(2, "size = 3\n");
    let before = tree.snapshot();
    let refused = tree.open(&at(NS, 1, &key("width", "integer"))).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::VersionConflict, "{refused}");
    assert_eq!(tree.snapshot(), before);
}

/// A downgrade past the kept edges is refused before writing anything.
#[test]
fn a_downgrade_past_the_kept_edges_writes_nothing() {
    let tree = Tree::new();
    tree.seed(1, "k1 = 1\n");
    let mut body = key("k5", "integer");
    for v in 1..5 {
        let op = format!(r#"{{ op = "rename", from = "k{v}", to = "k{}" }}"#, v + 1);
        body += &edge(v, v + 1, &[&op]);
    }
    drop(tree.open(&at(NS, 5, &body)).unwrap());
    let before = tree.snapshot();
    let refused = tree.open(&at(NS, 1, &key("k1", "integer"))).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::VersionConflict, "{refused}");
    assert_eq!(tree.snapshot(), before, "the file stays whole for v5");
    assert_eq!(tree.table()["k5"].as_integer(), Some(1));
}

// Files without a state.

/// A v1 build stamps its version on the first write, so v2 migrates it.
#[test]
fn a_file_a_v1_handle_wrote_is_migrated_by_v2() {
    let tree = Tree::new();
    let v1 = at(NS, 1, &key("width", "integer"));
    let old = tree.open(&v1).unwrap();
    assert!(!tree.state().exists(), "an open alone writes no state");
    old.set("width", Value::Integer(9)).unwrap();
    assert_eq!(tree.recorded(), (1, vec![]));
    drop(old);

    let v2 = at(
        NS,
        2,
        &(key("size", "integer")
            + &edge(1, 2, &[r#"{ op = "rename", from = "width", to = "size" }"#])),
    );
    let new = tree.open(&v2).unwrap();
    assert_eq!(new.get("size"), Some(int(9)));
    assert_eq!(tree.recorded(), (2, vec![(1, 2)]));
}

/// A later write keeps the version already recorded.
#[test]
fn a_write_keeps_the_recorded_version() {
    let tree = Tree::new();
    tree.seed(1, "width = 1\n");
    let v2 = at(
        NS,
        2,
        &(key("size", "integer")
            + &edge(1, 2, &[r#"{ op = "rename", from = "width", to = "size" }"#])),
    );
    let settings = tree.open(&v2).unwrap();
    settings.set("size", Value::Integer(2)).unwrap();
    assert_eq!(tree.recorded(), (2, vec![(1, 2)]));
}

/// A state left after the user file was deleted is not replayed later.
#[test]
fn a_stale_state_is_not_replayed_over_a_new_file() {
    let tree = Tree::new();
    tree.seed(1, "width = 1\n");
    fs::remove_file(tree.file()).unwrap();
    let settings = tree.open(&chain()).unwrap();
    settings.set("size", Value::String("12".into())).unwrap();
    drop(settings);
    let again = tree.open(&chain()).unwrap();
    assert_eq!(again.get("size"), Some(text("12")));
    assert_eq!(tree.recorded().0, 3);
}

/// A deleted file takes its state and last good copy with it.
#[test]
fn a_deleted_file_leaves_no_state_or_lastgood() {
    let tree = Tree::new();
    tree.seed(1, "width = 1\n");
    let lastgood = tree.dir().join("config.toml.lastgood");
    fs::write(&lastgood, "width = 1\n").unwrap();
    fs::remove_file(tree.file()).unwrap();
    tree.open(&chain()).unwrap();
    assert!(!tree.state().exists());
    assert!(
        !lastgood.exists(),
        "a later broken file can't fall back to it"
    );
}

/// A hand-written file without a state counts as current.
#[test]
fn a_hand_written_file_without_state_counts_as_current() {
    let tree = Tree::new();
    fs::write(tree.file(), "width = 9\n").unwrap();
    let v2 = at(
        NS,
        2,
        &(key("size", "integer")
            + &edge(1, 2, &[r#"{ op = "rename", from = "width", to = "size" }"#])),
    );
    let settings = tree.open(&v2).unwrap();
    assert_eq!(settings.get("size"), None, "not migrated");
    assert!(warned(&settings, "`width` is now `size`"));
    assert!(!tree.state().exists());
    assert_eq!(tree.text(), "width = 9\n");

    // The first write stamps the current version.
    settings.set("size", Value::Integer(1)).unwrap();
    assert_eq!(tree.recorded(), (2, vec![]));
}

// Broken files.

/// A migration over a broken file keeps it aside and migrates the last good copy.
#[test]
fn a_migration_over_a_broken_file_keeps_it_aside() {
    let tree = Tree::new();
    let v1 = at(NS, 1, &(key("width", "integer") + &key("name", "string")));
    let old = tree.open(&v1).unwrap();
    old.set("width", Value::Integer(30)).unwrap();
    old.set("name", Value::String("me".into())).unwrap();
    drop(old);
    fs::write(tree.file(), "width = [").unwrap();

    let body = key("size", "integer")
        + &key("name", "string")
        + &edge(1, 2, &[r#"{ op = "rename", from = "width", to = "size" }"#]);
    let settings = tree.open(&at(NS, 2, &body)).unwrap();
    let aside = tree.dir().join("config.toml.broken");
    assert_eq!(fs::read_to_string(aside).unwrap(), "width = [");
    assert_eq!(settings.get("size"), Some(int(30)));
    assert_eq!(settings.get("name"), Some(text("me")));
    assert!(
        warned(&settings, "last good copy was used"),
        "{:?}",
        settings.diagnostics()
    );
    assert_eq!(tree.table()["size"].as_integer(), Some(30));
    assert_eq!(tree.recorded().0, 2);
}

/// A broken file without a last good copy is left for later.
#[test]
fn a_broken_file_without_a_good_copy_is_left_alone() {
    let tree = Tree::new();
    tree.seed(1, "width = [");
    let before = tree.snapshot();
    let v2 = at(
        NS,
        2,
        &(key("size", "integer")
            + &edge(1, 2, &[r#"{ op = "rename", from = "width", to = "size" }"#])),
    );
    let settings = tree.open(&v2).unwrap();
    assert_eq!(settings.get("size"), None);
    let aside = tree.dir().join("config.toml.broken");
    assert_eq!(fs::read_to_string(aside).unwrap(), "width = [");
    let after: Vec<_> = tree
        .snapshot()
        .into_iter()
        .filter(|(p, _)| !p.ends_with("config.toml.broken"))
        .collect();
    assert_eq!(after, before, "the file and its state stay");
}

// Unknown keys.

/// Keys left unknown after a migration warn until a prune removes them.
#[test]
fn unknown_keys_after_a_migration_warn_until_pruned() {
    let tree = Tree::new();
    tree.seed(1, "width = 3\nstale = true\n[old]\nthing = 1\n");
    let v2 = at(
        NS,
        2,
        &(key("size", "integer")
            + &edge(1, 2, &[r#"{ op = "rename", from = "width", to = "size" }"#])),
    );
    let settings = tree.open(&v2).unwrap();
    assert_eq!(settings.get("size"), Some(int(3)));
    assert!(warned(&settings, "unknown key `stale`"));
    assert!(warned(&settings, "unknown key `old.thing`"));
    assert!(tree.table().contains_key("stale"), "kept");

    let removed = settings.prune().unwrap();
    assert_eq!(removed, ["old.thing", "stale"]);
    let table = tree.table();
    assert!(!table.contains_key("stale"), "{table:?}");
    assert_eq!(table["size"].as_integer(), Some(3));
    assert!(
        !warned(&settings, "unknown key"),
        "{:?}",
        settings.diagnostics()
    );
}

// Idempotence.

/// Each file in the user directory with its bytes and mtime.
fn stamps(tree: &Tree) -> Vec<(PathBuf, Vec<u8>, SystemTime)> {
    let mtime = |p: &PathBuf| fs::metadata(p).unwrap().modified().unwrap();
    let stamp = |(p, bytes): (PathBuf, Vec<u8>)| {
        let at = mtime(&p);
        (p, bytes, at)
    };
    tree.snapshot().into_iter().map(stamp).collect()
}

/// Reopening a migrated file writes nothing.
#[test]
fn reopening_a_migrated_file_writes_nothing() {
    let tree = Tree::new();
    tree.seed(1, "theme = \"night\"\n[ui]\nwidth = 640\n");
    fs::write(tree.dir().join("layout.yaml"), "mine").unwrap();
    let first = tree.open(&undoable_v3()).unwrap();
    let values: Vec<_> = first
        .resolved()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    drop(first);
    let before = stamps(&tree);

    // A coarse mtime clock could hide a rewrite, so wait past a tick.
    std::thread::sleep(std::time::Duration::from_millis(20));
    let again = tree.open(&undoable_v3()).unwrap();
    let now: Vec<_> = again
        .resolved()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    assert_eq!(now, values);
    assert_eq!(stamps(&tree), before);
    assert!(errors(&again).is_empty(), "{:?}", again.diagnostics());
}

// Shared namespaces.

/// A shared namespace migrates `conf.toml` and its own state.
#[test]
fn a_shared_namespace_migrates_conf_toml() {
    const SHARED: &str = "org.example.desktop.theme";
    let tmp = tempfile::tempdir().unwrap();
    let roots = Roots::under(tmp.path());
    let dir = roots
        .user_dir
        .as_ref()
        .unwrap()
        .join("mxconfig")
        .join(SHARED);
    common::put(&dir.join("conf.toml"), "tint = \"red\"\n");
    fs::write(dir.join("conf.toml.state"), "version = 1\n").unwrap();
    let schema = format!(
        "namespace = \"{SHARED}\"\nversion = 2\n{}{}",
        key("accent", "string"),
        edge(
            1,
            2,
            &[r#"{ op = "rename", from = "tint", to = "accent" }"#]
        )
    );
    common::install(&roots, SHARED, &schema);
    // Only the session daemon opens a shared namespace's files.
    let roots = Roots {
        serving: Some(Bus::Session),
        ..roots
    };
    let settings = common::open(&Source::with_roots(roots), SHARED).unwrap();
    assert_eq!(settings.get("accent"), Some(text("red")));
    let recorded = common::recorded(&dir.join("conf.toml")).unwrap();
    assert_eq!(recorded.0, 2);
}

// A serving source holds a schema while open, and migrates on the next open.

/// A source that serves clients over `tree`, as mxconfigd does.
fn serving(tree: &Tree) -> Source {
    Source::with_roots(Roots {
        serving: Some(Bus::Session),
        ..tree.roots.clone()
    })
}

/// An installed update leaves the open namespace alone, and the next open migrates.
#[test]
fn a_serving_source_crosses_several_edges_on_the_next_open() {
    let tree = Tree::new();
    tree.install(&at(NS, 1, &key("width", "integer")));
    let source = serving(&tree);
    let settings = common::open(&source, NS).unwrap();
    settings.set("width", Value::Integer(12)).unwrap();
    source.outcomes();

    source.touched(&tree.install(&chain()));
    assert!(!detached(&source.outcomes(), NS));
    assert_eq!(settings.describe().version(), 1, "held while open");
    assert_eq!(settings.get("width"), Some(int(12)));
    assert_eq!(tree.recorded(), (1, vec![]), "the file is not migrated yet");

    drop(settings);
    let settings = common::open(&source, NS).unwrap();
    assert_eq!(settings.describe().version(), 3);
    assert_eq!(settings.get("size"), Some(text("12")));
    assert_eq!(settings.get("width"), None);
    assert_eq!(tree.recorded(), (3, vec![(1, 2), (2, 3)]));
    assert_eq!(tree.table()["size"].as_str(), Some("12"));

    // A schema that no longer parses refuses the next open.
    tree.install("namespace = ");
    drop(settings);
    assert!(common::open(&source, NS).is_err());
}

/// An installed older schema moves the namespace back down on the next open.
#[test]
fn a_serving_source_downgrades_on_the_next_open() {
    let tree = Tree::new();
    tree.seed(1, "width = 5\n");
    tree.install(&chain());
    let source = serving(&tree);
    let settings = common::open(&source, NS).unwrap();
    assert_eq!(settings.get("size"), Some(text("5")));

    source.touched(&tree.install(&at(NS, 1, &key("width", "integer"))));
    assert_eq!(settings.get("size"), Some(text("5")), "held while open");

    drop(settings);
    let settings = common::open(&source, NS).unwrap();
    assert_eq!(settings.get("width"), Some(int(5)));
    assert_eq!(tree.recorded(), (1, vec![]));
}

/// An update whose migration fails refuses the next open and keeps the file.
#[test]
fn a_failed_migration_refuses_the_next_open() {
    let tree = Tree::new();
    tree.install(&at(NS, 1, &key("count", "string")));
    let source = serving(&tree);
    let settings = common::open(&source, NS).unwrap();
    settings.set("count", Value::String("many".into())).unwrap();
    let before = tree.snapshot();

    let body = key("count", "integer")
        + &edge(
            1,
            2,
            &[r#"{ op = "retype", key = "count", from = "string", to = "integer" }"#],
        );
    source.touched(&tree.install(&at(NS, 2, &body)));
    assert_eq!(settings.get("count"), Some(text("many")), "held while open");

    drop(settings);
    let e = common::open(&source, NS).unwrap_err();
    assert!(e.to_string().contains("cannot coerce `count`"), "{e}");
    assert_eq!(tree.snapshot(), before);
}

/// A newer embedded schema migrates through several edges on the next open.
#[test]
fn a_serving_source_takes_a_newer_embedded_chain_on_the_next_open() {
    let tree = Tree::new();
    let source = serving(&tree);
    source.embed(NS, &at(NS, 1, &key("width", "integer")));
    let settings = common::open(&source, NS).unwrap();
    settings.set("width", Value::Integer(4)).unwrap();
    source.outcomes();

    source.embed(NS, &chain());
    assert!(!detached(&source.outcomes(), NS));
    assert_eq!(settings.describe().version(), 1, "held while open");
    assert_eq!(settings.get("width"), Some(int(4)), "held while open");

    drop(settings);
    let settings = common::open(&source, NS).unwrap();
    assert_eq!(settings.get("size"), Some(text("4")));
    assert_eq!(tree.recorded(), (3, vec![(1, 2), (2, 3)]));
}

/// A retired key warns as retired, and a prune removes it and the retired asset files.
#[test]
fn retired_names_warn_and_are_pruned() {
    let tree = Tree::new();
    tree.seed(1, "eq = \"rock\"\n");
    fs::write(tree.dir().join("cache.bin"), "x").unwrap();
    let retired = "[retired]\nkeys = [\"eq\"]\nassets = [\"cache.bin\"]\n";
    let settings = tree
        .open(&at(NS, 1, &(key("size", "integer") + retired)))
        .unwrap();
    assert!(warned(&settings, "`eq` is retired"));
    assert_eq!(settings.prune().unwrap(), ["asset cache.bin", "eq"]);
    assert!(!tree.dir().join("cache.bin").exists());
    assert!(!tree.table().contains_key("eq"));
}

/// A system file still using old names says what they are now, through every rename.
#[test]
fn a_system_file_using_old_names_says_what_they_are_now() {
    let tree = Tree::new();
    let admin = tree.roots.admin_dir.as_ref().unwrap().join(NS);
    fs::create_dir_all(&admin).unwrap();
    fs::write(admin.join("config.toml"), "width = 3\n").unwrap();
    fs::write(admin.join("layout.yaml"), "old").unwrap();
    let body = key("length", "integer")
        + "[assets.layout]\npath = \"layout.kdl\"\n"
        + &edge(
            1,
            2,
            &[
                r#"{ op = "rename", from = "width", to = "size" }"#,
                r#"{ op = "rename-asset", from = "layout.yaml", to = "layout.kdl" }"#,
            ],
        )
        + &edge(
            2,
            3,
            &[r#"{ op = "rename", from = "size", to = "length" }"#],
        );
    let settings = tree.open(&at(NS, 3, &body)).unwrap();
    assert!(warned(&settings, "`width` is now `length`"));
    assert!(warned(
        &settings,
        "asset file `layout.yaml` is now `layout.kdl`"
    ));
    assert_eq!(
        settings.get("length"),
        None,
        "system files are never migrated"
    );
}
