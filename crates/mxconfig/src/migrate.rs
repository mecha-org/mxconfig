//! Schema migrations: strict upgrades, best-effort downgrades, and pruning.

use std::fs;
use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item, value};

use crate::error::{Diagnostic, Error, Kind};
use crate::resolve;
use crate::write;
use mxconfig_schema::{KeyType, Migration, Op, Schema};
use serde::{Deserialize, Serialize};

/// The number of applied edges kept in the state file.
const KEEP_EDGES: usize = 3;

/// A file's migration state: its version and the edges applied to it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct State {
    version: u32,
    #[serde(default)]
    applied: Vec<Migration>,
}

/// Read the state file. A missing or unreadable one counts as empty.
fn load_state(path: &Path) -> State {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or_default()
}

/// Write the state file atomically.
fn save_state(path: &Path, state: &State) -> Result<(), Error> {
    let text = toml::to_string(state).map_err(|e| Kind::Migration(e.to_string()))?;
    write::atomic_write(path, text.as_bytes())
}

/// The version recorded for `target`, if it has a state file.
pub(crate) fn file_version(target: &Path) -> Option<u32> {
    let path = write::state_path(target);
    path.exists().then(|| load_state(&path).version)
}

/// The edges recorded for `target`, newest last.
pub(crate) fn recorded(target: &Path) -> Vec<Migration> {
    load_state(&write::state_path(target)).applied
}

/// Whether `target` exists at another version than `schema`'s.
pub(crate) fn needs_migrating(target: &Path, schema: &Schema) -> bool {
    target.exists() && file_version(target).is_some_and(|version| version != schema.version())
}

/// Record `schema`'s version for `target`, unless a version is recorded.
pub(crate) fn stamp(target: &Path, schema: &Schema) -> Result<(), Error> {
    let path = write::state_path(target);
    if path.exists() {
        return Ok(());
    }
    let state = State {
        version: schema.version(),
        ..State::default()
    };
    save_state(&path, &state)
}

/// An asset file move: its path before and after.
type Move = (String, String);

/// Migrate a document up to `schema`'s version, planning asset moves in `assets`.
fn migrate_up(
    doc: &mut DocumentMut,
    assets: &mut Vec<Move>,
    state: &mut State,
    schema: &Schema,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let to = schema.version();
    while state.version < to {
        let current = state.version;
        let Some(edge) = schema.edge_from(current) else {
            diagnostics.push(Diagnostic::error(format!(
                "no migration path from v{current} to v{to}"
            )));
            return;
        };
        for op in &edge.ops {
            apply(op, doc, assets, diagnostics, false);
        }
        record_edge(state, edge);
        state.version = edge.to;
    }
}

/// Migrate a document down to version `to`, using the recorded edges.
fn migrate_down(
    doc: &mut DocumentMut,
    assets: &mut Vec<Move>,
    state: &mut State,
    to: u32,
    diagnostics: &mut Vec<Diagnostic>,
) {
    while state.version > to {
        let current = state.version;
        let found = state
            .applied
            .iter()
            .position(|m| m.to == current && m.from >= to);
        // Without a recorded step the caller refuses the file at its version.
        let Some(edge) = found.map(|i| state.applied.remove(i)) else {
            return;
        };
        for op in edge.ops.iter().rev() {
            apply(&op.inverse(), doc, assets, diagnostics, true);
        }
        state.version = edge.from;
    }
}

/// Remove keys the schema does not define. Returns the removed paths.
pub(crate) fn prune(doc: &mut DocumentMut, schema: &Schema) -> Vec<String> {
    let Ok(plain) = toml::from_str::<toml::Value>(&doc.to_string()) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    resolve::collect(&plain, schema, "", &mut entries);
    let mut removed: Vec<String> = entries
        .into_iter()
        .filter_map(|(path, key, _)| key.is_none().then_some(path))
        .collect();
    for path in &removed {
        write::remove_path(doc, path);
    }
    removed.sort();
    removed
}

/// Remove the retired asset files beside `target`. Returns them as `asset <path>`.
pub(crate) fn prune_assets(target: &Path, schema: &Schema) -> Result<Vec<String>, Error> {
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    let mut removed = Vec::new();
    for path in &schema.retired().assets {
        let file = dir.join(path);
        if file.is_file() {
            fs::remove_file(&file).map_err(|e| Error::io(&file, e))?;
            removed.push(format!("asset {path}"));
        }
    }
    Ok(removed)
}

/// Remove the state and last good copy of a file that is gone.
fn forget(target: &Path, notes: &mut Vec<Diagnostic>) {
    for path in [write::state_path(target), write::lastgood_path(target)] {
        if let Err(e) = write::remove_if_present(&path) {
            let path = path.display();
            notes.push(Diagnostic::warning(format!("cannot remove {path}: {e}")));
        }
    }
}

/// Migrate the file at `target` to `schema`'s version. Returns notes.
pub(crate) fn bring_up_to_date(target: &Path, schema: &Schema) -> Result<Vec<Diagnostic>, Error> {
    let mut notes = Vec::new();
    // A missing file leaves nothing for its state to describe.
    if !target.exists() {
        forget(target, &mut notes);
        return Ok(notes);
    }
    // A file without a state file is treated as current.
    let state_path = write::state_path(target);
    if !state_path.exists() {
        return Ok(notes);
    }
    let mut state = load_state(&state_path);
    let file_version = state.version;
    if file_version == schema.version() {
        return Ok(notes);
    }
    let Some(mut doc) = write::load_doc(target) else {
        return Ok(notes);
    };
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    let mut assets = Vec::new();
    let upward = file_version < schema.version();
    if upward {
        migrate_up(&mut doc, &mut assets, &mut state, schema, &mut notes);
    } else {
        let to = schema.version();
        migrate_down(&mut doc, &mut assets, &mut state, to, &mut notes);
    }
    fail_on_error(&notes)?;
    // A downgrade that can't get all the way leaves the file whole for the newer build.
    if state.version != schema.version() {
        let (file, own) = (file_version, schema.version());
        let why = format!(
            "{} is at schema v{file}, with no recorded steps down to v{own}",
            target.display()
        );
        let other = Some(file);
        return Err(Kind::VersionConflict { why, other }.into());
    }
    // Assets move only once every edge has succeeded.
    move_assets(&assets, dir, !upward, &mut notes);
    fail_on_error(&notes)?;
    if let Some(aside) = write::write_doc(target, &doc)? {
        notes.push(crate::space::aside_note(target, &aside));
    }
    save_state(&state_path, &state)?;
    Ok(notes)
}

/// Turn the first error in `notes` into a migration error.
fn fail_on_error(notes: &[Diagnostic]) -> Result<(), Error> {
    match notes.iter().find(|d| d.is_error()) {
        Some(bad) => Err(Kind::Migration(bad.message().to_string()).into()),
        None => Ok(()),
    }
}

/// The diagnostic for a failed op. Best effort only warns.
fn failure(best_effort: bool, message: String) -> Diagnostic {
    if best_effort {
        Diagnostic::warning(message)
    } else {
        Diagnostic::error(message)
    }
}

/// Apply one operation to `doc`. Asset moves are queued in `assets`.
fn apply(
    op: &Op,
    doc: &mut DocumentMut,
    assets: &mut Vec<Move>,
    diags: &mut Vec<Diagnostic>,
    best_effort: bool,
) {
    match op {
        Op::Rename { from, to } => match write::remove_path(doc, from) {
            Some(item) => write::set_path(doc, to, item),
            None => diags.push(Diagnostic::warning(format!("`{from}` is not present"))),
        },
        Op::Retype { key, from, to } => {
            let Some(item) = write::get_path(doc, key) else {
                return;
            };
            match coerce(item, from, to) {
                Some(new) => write::set_path(doc, key, new),
                None => diags.push(failure(
                    best_effort,
                    format!("cannot coerce `{key}` from {from} to {to}"),
                )),
            }
        }
        Op::Remap { key, map } => {
            let Some(text) = write::get_path(doc, key).and_then(Item::as_str) else {
                return;
            };
            match map.get(text) {
                Some(new) => write::set_path(doc, key, value(new.clone())),
                None => diags.push(Diagnostic::warning(format!("`{text}` is not in the map"))),
            }
        }
        Op::RenameAsset { from, to } => assets.push((from.clone(), to.clone())),
    }
}

/// Move asset files in order. An error puts moved files back.
fn move_assets(planned: &[Move], dir: &Path, best_effort: bool, diags: &mut Vec<Diagnostic>) {
    let mut moved = Vec::new();
    for (from, to) in planned {
        match move_asset(from, to, dir, best_effort) {
            Ok(paths) => moved.push(paths),
            Err(bad) if bad.is_error() => {
                diags.push(bad);
                for (source, target) in moved.iter().rev() {
                    let _ = fs::rename(target, source);
                }
                return;
            }
            Err(bad) => diags.push(bad),
        }
    }
}

/// Move asset file `from` to `to`. Returns the source and target.
fn move_asset(
    from: &str,
    to: &str,
    dir: &Path,
    best_effort: bool,
) -> Result<(PathBuf, PathBuf), Diagnostic> {
    let source = dir.join(from);
    let target = dir.join(to);
    if !source.is_file() {
        return Err(Diagnostic::warning(format!(
            "asset `{from}` is not present"
        )));
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            failure(
                best_effort,
                format!("cannot create {}: {e}", parent.display()),
            )
        })?;
    }
    fs::rename(&source, &target)
        .map_err(|e| failure(best_effort, format!("cannot move asset `{from}`: {e}")))?;
    Ok((source, target))
}

/// Convert an item between key types, if there is an obvious conversion.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "a retype between numbers converts as `as` does"
)]
fn coerce(item: &Item, from: &KeyType, to: &KeyType) -> Option<Item> {
    if from == to {
        return Some(item.clone());
    }
    Some(match (from, to) {
        (KeyType::Integer, KeyType::Float) => value(item.as_integer()? as f64),
        (KeyType::Float, KeyType::Integer) => value(item.as_float()? as i64),
        (KeyType::Integer, KeyType::String) => value(item.as_integer()?.to_string()),
        (KeyType::Float, KeyType::String) => value(item.as_float()?.to_string()),
        (KeyType::Bool, KeyType::String) => value(item.as_bool()?.to_string()),
        (KeyType::String, KeyType::Integer) => value(item.as_str()?.parse::<i64>().ok()?),
        (KeyType::String, KeyType::Float) => value(item.as_str()?.parse::<f64>().ok()?),
        (KeyType::String, KeyType::Bool) => value(item.as_str()?.parse::<bool>().ok()?),
        _ => return None,
    })
}

/// Record an applied edge, keeping only the newest ones.
fn record_edge(state: &mut State, edge: &Migration) {
    let step = (edge.from, edge.to);
    state.applied.retain(|m| (m.from, m.to) != step);
    state.applied.push(edge.clone());
    let excess = state.applied.len().saturating_sub(KEEP_EDGES);
    state.applied.drain(..excess);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(from: u32, to: u32) -> Migration {
        Migration {
            from,
            to,
            ops: vec![Op::Rename {
                from: format!("k{from}"),
                to: format!("k{to}"),
            }],
        }
    }

    #[test]
    fn the_log_keeps_the_newest_edges_and_drains_as_it_is_consumed() {
        let mut state = State::default();
        for i in 1..=5 {
            record_edge(&mut state, &edge(i, i + 1));
        }
        let kept: Vec<(u32, u32)> = state.applied.iter().map(|m| (m.from, m.to)).collect();
        assert_eq!(kept, vec![(3, 4), (4, 5), (5, 6)]);

        // Each downgrade step removes its edge.
        state.version = 6;
        let (mut doc, mut assets, mut notes) = (DocumentMut::new(), Vec::new(), Vec::new());
        migrate_down(&mut doc, &mut assets, &mut state, 5, &mut notes);
        assert_eq!(state.applied.len(), 2);
        migrate_down(&mut doc, &mut assets, &mut state, 3, &mut notes);
        assert_eq!(state.version, 3);
        assert!(state.applied.is_empty(), "{:?}", state.applied);
    }

    #[test]
    fn a_failed_asset_move_puts_earlier_moves_back() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        fs::write(dir.join("a.txt"), "a").unwrap();
        fs::write(dir.join("b.txt"), "b").unwrap();
        // A rename onto a non-empty directory fails.
        fs::create_dir_all(dir.join("full/x")).unwrap();
        let rename = |from: &str, to: &str| (from.to_string(), to.to_string());
        let ops = [rename("a.txt", "moved.txt"), rename("b.txt", "full")];
        let mut notes = Vec::new();
        move_assets(&ops, dir, false, &mut notes);
        assert!(notes.iter().any(Diagnostic::is_error), "{notes:?}");
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "a");
        assert!(!dir.join("moved.txt").exists());
        assert_eq!(fs::read_to_string(dir.join("b.txt")).unwrap(), "b");

        // Best effort only warns and keeps going.
        let mut notes = Vec::new();
        move_assets(&ops, dir, true, &mut notes);
        assert!(!notes.iter().any(Diagnostic::is_error), "{notes:?}");
        assert_eq!(fs::read_to_string(dir.join("moved.txt")).unwrap(), "a");
    }
}
