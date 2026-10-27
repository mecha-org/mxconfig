//! Reading, editing and atomically writing TOML files.

use std::fs::{self, File};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use toml_edit::{DocumentMut, InlineTable, Item, Table, TableLike, Value};

use crate::error::Error;

/// Suffix of a file's last good copy.
const LASTGOOD_SUFFIX: &str = ".lastgood";

/// Suffix of a saved copy of a broken file.
const BROKEN_SUFFIX: &str = ".broken";

/// Suffix of a file's migration state.
const STATE_SUFFIX: &str = ".state";

/// The path of a file's last good copy.
pub(crate) fn lastgood_path(path: &Path) -> PathBuf {
    with_suffix(path, LASTGOOD_SUFFIX)
}

/// The path of a file's migration state.
pub(crate) fn state_path(path: &Path) -> PathBuf {
    with_suffix(path, STATE_SUFFIX)
}

pub(crate) fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut os = path.as_os_str().to_owned();
    os.push(suffix);
    PathBuf::from(os)
}

/// Read the last good copy, if it exists and parses.
fn read_lastgood(path: &Path) -> Option<DocumentMut> {
    fs::read_to_string(lastgood_path(path))
        .ok()?
        .parse::<DocumentMut>()
        .ok()
}

/// Read a file for editing, or its last good copy if it doesn't parse.
pub(crate) fn load_doc(path: &Path) -> Option<DocumentMut> {
    let text = fs::read_to_string(path).ok()?;
    text.parse().ok().or_else(|| read_lastgood(path))
}

/// Write a document atomically, copying a broken file aside first.
pub(crate) fn write_doc(path: &Path, doc: &DocumentMut) -> Result<Option<PathBuf>, Error> {
    let text = fs::read_to_string(path).unwrap_or_default();
    let broken = text.parse::<DocumentMut>().is_err();
    let aside = broken.then(|| keep_aside(path, &text)).transpose()?;
    commit(path, doc)?;
    Ok(aside)
}

/// Save the broken `text` of `path` to `<path>.broken`.
pub(crate) fn keep_aside(path: &Path, text: &str) -> Result<PathBuf, Error> {
    let aside = with_suffix(path, BROKEN_SUFFIX);
    write_if_new(&aside, text)?;
    Ok(aside)
}

/// Save `text` as the last good copy of `path`.
pub(crate) fn snapshot(path: &Path, text: &str) -> Result<(), Error> {
    write_if_new(&lastgood_path(path), text)
}

/// Write `text` to `path` unless it already contains it.
fn write_if_new(path: &Path, text: &str) -> Result<(), Error> {
    match fs::read(path) {
        Ok(held) if held == text.as_bytes() => Ok(()),
        _ => replace(path, text.as_bytes(), false),
    }
}

/// Edit a file atomically under the caller's write lock, returning where a broken file was kept.
pub(crate) fn edit(
    path: &Path,
    mutate: impl FnOnce(&mut DocumentMut),
) -> Result<Option<PathBuf>, Error> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(Error::io(path, e)),
    };
    let (mut doc, aside) = if let Ok(doc) = text.parse::<DocumentMut>() {
        (doc, None)
    } else {
        let aside = keep_aside(path, &text)?;
        (read_lastgood(path).unwrap_or_default(), Some(aside))
    };
    mutate(&mut doc);
    commit(path, &doc)?;
    Ok(aside)
}

/// Replace `path` with `bytes` atomically, keeping an existing file's mode.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    // One sync: a crash may lose the latest write, but never leaves a partial file.
    replace(path, bytes, true)
}

/// Replace `path` with `bytes` atomically, making its directory if needed.
fn replace(path: &Path, bytes: &[u8], sync: bool) -> Result<(), Error> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
    let (tmp, mut file) = new_file_in(dir).map_err(|e| Error::io(path, e))?;
    let written = (|| {
        if let Ok(meta) = fs::metadata(path) {
            file.set_permissions(meta.permissions())?;
        }
        file.write_all(bytes)?;
        if sync {
            file.sync_data()?;
        }
        fs::rename(&tmp, path)
    })();
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(Error::io(path, e));
    }
    Ok(())
}

/// Create a new temporary file in `dir`.
fn new_file_in(dir: &Path) -> std::io::Result<(PathBuf, File)> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    loop {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let tmp = dir.join(format!(".tmp{}-{n}", std::process::id()));
        // Skip names left behind by an earlier process with the same pid.
        match File::create_new(&tmp) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            result => return result.map(|file| (tmp, file)),
        }
    }
}

/// Remove `path`, ignoring a missing file.
pub(crate) fn remove_if_present(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// Write a document, then its unsynced last good copy.
fn commit(path: &Path, doc: &DocumentMut) -> Result<(), Error> {
    let text = doc.to_string();
    atomic_write(path, text.as_bytes())?;
    replace(&lastgood_path(path), text.as_bytes(), false)
}

/// A TOML value as an editable item.
pub(crate) fn to_item(value: &toml::Value) -> Item {
    Item::Value(
        value
            .serialize(toml_edit::ser::ValueSerializer::new())
            .expect("a TOML value always re-serializes"),
    )
}

/// The item at a dotted path, if present.
pub(crate) fn get_path<'a>(doc: &'a DocumentMut, path: &str) -> Option<&'a Item> {
    let mut parts = path.split('.');
    parts.try_fold(doc.as_item(), |item, part| item.as_table_like()?.get(part))
}

/// Set a dotted key, keeping the old value's formatting.
pub(crate) fn set_path(doc: &mut DocumentMut, path: &str, value: Item) {
    let mut parts = path.split('.');
    let last = parts.next_back().expect("split yields at least one part");
    let mut table: &mut dyn TableLike = doc.as_table_mut();
    let mut inline = false;
    for part in parts {
        if table.get(part).is_none_or(|item| !item.is_table_like()) {
            // An inline table can only hold inline tables.
            let group = if inline {
                Item::Value(InlineTable::new().into())
            } else {
                Item::Table(Table::new())
            };
            insert(table, part, group, inline);
        }
        let item = table.get_mut(part).expect("just made a table");
        inline = item.is_inline_table();
        table = item.as_table_like_mut().expect("just made a table");
    }
    insert(table, last, value, inline);
}

/// Insert `value` at `key`, keeping the formatting around it.
fn insert(table: &mut dyn TableLike, key: &str, mut value: Item, inline: bool) {
    if let Some(new) = value.as_value_mut() {
        if let Some(old) = table.get(key).and_then(Item::as_value) {
            // Keep formatting such as an inline comment.
            *new.decor_mut() = old.decor().clone();
        } else if inline && let Some(end) = last_value(table) {
            pass_suffix(end, new);
        }
    }
    table.insert(key, value);
}

/// Remove a dotted key. Returns the removed item.
pub(crate) fn remove_path(doc: &mut DocumentMut, path: &str) -> Option<Item> {
    let mut parts = path.split('.');
    let last = parts.next_back()?;
    let mut table: &mut dyn TableLike = doc.as_table_mut();
    let mut inline = false;
    for part in parts {
        let item = table.get_mut(part)?;
        inline = item.is_inline_table();
        table = item.as_table_like_mut()?;
    }
    let was_end = table.iter().last().is_some_and(|(key, _)| key == last);
    let mut item = table.remove(last)?;
    if inline
        && was_end
        && let Some(gone) = item.as_value_mut()
        && let Some(end) = last_value(table)
    {
        pass_suffix(gone, end);
    }
    Some(item)
}

/// The last entry of `table`, if it is a value.
fn last_value(table: &mut dyn TableLike) -> Option<&mut Value> {
    table.iter_mut().last()?.1.as_value_mut()
}

/// Move the spacing before an inline table's closing brace to `to`.
fn pass_suffix(from: &mut Value, to: &mut Value) {
    if let Some(suffix) = from.decor().suffix().cloned() {
        from.decor_mut().set_suffix("");
        to.decor_mut().set_suffix(suffix);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(path: &Path, key: &str, value: &toml::Value) {
        edit(path, |doc| set_path(doc, key, to_item(value))).unwrap();
    }

    #[test]
    fn an_edit_keeps_comments_and_refreshes_the_lastgood_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conf.toml");
        fs::write(&path, "# keep me\n[colors]\naccent = \"#000000\"\n").unwrap();

        set(
            &path,
            "colors.accent",
            &toml::Value::String("#ff0000".into()),
        );
        set(&path, "colors.corner_radius", &toml::Value::Integer(20));
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("# keep me"), "{text}");
        assert!(text.contains("accent = \"#ff0000\""), "{text}");
        assert!(text.contains("corner_radius = 20"), "{text}");
        let lastgood = read_lastgood(&path).expect("a snapshot exists");
        assert_eq!(lastgood["colors"]["accent"].as_str(), Some("#ff0000"));

        edit(&path, |doc| {
            remove_path(doc, "colors.corner_radius");
        })
        .unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("corner_radius"), "{text}");
    }

    #[test]
    fn a_write_into_an_inline_table_keeps_it_inline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conf.toml");
        fs::write(&path, "colors = { accent = \"red\", mode = \"dark\" }\n").unwrap();
        set(&path, "colors.mode", &toml::Value::String("light".into()));
        set(&path, "colors.radius", &toml::Value::Integer(4));
        set(&path, "colors.panel.width", &toml::Value::Integer(9));
        let text = fs::read_to_string(&path).unwrap();
        let want =
            "colors = { accent = \"red\", mode = \"light\", radius = 4, panel = { width = 9 } }\n";
        assert_eq!(text, want);

        let doc = load_doc(&path).unwrap();
        assert_eq!(
            get_path(&doc, "colors.panel.width").unwrap().as_integer(),
            Some(9)
        );
        edit(&path, |doc| {
            remove_path(doc, "colors.panel");
        })
        .unwrap();
        let want = "colors = { accent = \"red\", mode = \"light\", radius = 4 }\n";
        assert_eq!(fs::read_to_string(&path).unwrap(), want);
    }
}

#[cfg(test)]
mod modes {
    use std::os::unix::fs::PermissionsExt;

    /// A replaced file keeps its mode, so admin files stay readable.
    #[test]
    fn a_replaced_file_keeps_its_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conf.toml");
        std::fs::write(&path, "a = 1\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        super::atomic_write(&path, b"a = 2\n").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a = 2\n");
        let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(left.len(), 1, "no temp file left behind");
    }
}
