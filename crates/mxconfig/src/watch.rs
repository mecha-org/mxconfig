//! Inotify watches on the directories a [`crate::Source`] reads.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use inotify::{EventMask, Inotify, WatchDescriptor, WatchMask};

use crate::error::{Diagnostic, Error};
use crate::paths;

/// The events that can change what a namespace reads.
const MASK: WatchMask = WatchMask::CREATE
    .union(WatchMask::CLOSE_WRITE)
    .union(WatchMask::MOVED_TO)
    .union(WatchMask::MOVED_FROM)
    .union(WatchMask::DELETE)
    .union(WatchMask::ATTRIB);

/// Only entries appearing, disappearing or being renamed. For a layer's base.
const ENTRIES: WatchMask = WatchMask::CREATE
    .union(WatchMask::MOVED_TO)
    .union(WatchMask::MOVED_FROM)
    .union(WatchMask::DELETE)
    .union(WatchMask::ONLYDIR);

/// How much of a directory is watched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Reach {
    /// Its entries appearing, disappearing or being renamed.
    Entries,
    /// Those, plus files written in it.
    Full,
}

impl Reach {
    fn mask(self) -> WatchMask {
        match self {
            Reach::Entries => ENTRIES,
            Reach::Full => MASK,
        }
    }
}

/// Directories to watch, and how much of each.
pub(crate) type Targets = BTreeMap<PathBuf, Reach>;

/// Size of an `inotify_event` header: wd, mask, cookie and name length.
const HEADER: usize = 16;

/// One decoded inotify event.
pub(crate) enum Event {
    /// Something changed at this path.
    Touched(PathBuf),
    /// The queue overflowed and events were lost.
    Overflow,
}

/// One inotify instance and the directories it watches.
pub(crate) struct Watches {
    inotify: Inotify,
    /// Watched directories by watch descriptor id.
    watched: BTreeMap<i32, (PathBuf, Reach, WatchDescriptor)>,
}

impl Watches {
    pub(crate) fn new() -> Result<Watches, Error> {
        Ok(Watches {
            inotify: Inotify::init().map_err(|e| Error::io("<inotify>", e))?,
            watched: BTreeMap::new(),
        })
    }

    /// Stop watching every directory not in `wanted`.
    pub(crate) fn retain(&mut self, wanted: &Targets) {
        let mut watches = self.inotify.watches();
        let gone = self
            .watched
            .extract_if(.., |_, (dir, _, _)| !wanted.contains_key(dir));
        for (_, (_, _, wd)) in gone {
            // Fails only if the kernel already removed the watch.
            let _ = watches.remove(wd);
        }
    }

    /// Watch `dirs` that are new or need another reach. Returns a warning per failure.
    pub(crate) fn add(&mut self, dirs: &Targets) -> Vec<Diagnostic> {
        let mut failed = Vec::new();
        for (dir, &reach) in dirs {
            let same = |(watched, had, _): &(PathBuf, Reach, WatchDescriptor)| {
                watched == dir && *had == reach
            };
            if self.watched.values().any(same) {
                continue;
            }
            // Adding the same directory again replaces its mask, under the same id.
            match self.inotify.watches().add(dir, reach.mask()) {
                Ok(wd) => {
                    let id = wd.get_watch_descriptor_id();
                    self.watched.insert(id, (dir.clone(), reach, wd));
                }
                Err(e) => failed.push(Diagnostic::warning(format!(
                    "cannot watch {}: {e}",
                    dir.display()
                ))),
            }
        }
        failed
    }

    /// Decode the events in `bytes`, read from the inotify fd.
    pub(crate) fn decode(&mut self, mut bytes: &[u8]) -> Vec<Event> {
        let mut events = Vec::new();
        while bytes.len() >= HEADER {
            let word = |at: usize| u32::from_ne_bytes(bytes[at..at + 4].try_into().unwrap());
            let (wd, len) = (word(0).cast_signed(), word(12) as usize);
            let mask = EventMask::from_bits_retain(word(4));
            let end = (HEADER + len).min(bytes.len());
            let name = bytes[HEADER..end]
                .split(|&b| b == 0)
                .next()
                .unwrap_or_default();
            bytes = &bytes[end..];
            if mask.contains(EventMask::Q_OVERFLOW) {
                events.push(Event::Overflow);
                continue;
            }
            // The kernel removed the watch, e.g. its directory was deleted.
            if mask.contains(EventMask::IGNORED) {
                if let Some((dir, _, _)) = self.watched.remove(&wd) {
                    events.push(Event::Touched(dir));
                }
                continue;
            }
            let Some((dir, _, _)) = self.watched.get(&wd) else {
                continue; // a watch already let go
            };
            let path = dir.join(OsStr::from_bytes(name));
            // A new file waits for its close-write unless it is already whole.
            let new_file = mask.contains(EventMask::CREATE) && !mask.contains(EventMask::ISDIR);
            if !new_file || whole(&path) {
                events.push(Event::Touched(path));
            }
        }
        events
    }

    pub(crate) fn fd(&self) -> BorrowedFd<'_> {
        self.inotify.as_fd()
    }
}

/// Whether a new file is complete now, i.e. a symlink or hard link.
fn whole(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(path);
    meta.is_ok_and(|m| m.file_type().is_symlink() || m.nlink() > 1)
}

/// A directory a namespace reads, and the entries in it that matter.
#[derive(Debug, Clone)]
pub(crate) struct Need {
    /// The highest ancestor that may be watched in its place.
    pub(crate) bound: PathBuf,
    /// The entries read in it, or `None` for every `*.toml`.
    names: Option<BTreeSet<OsString>>,
    /// Where to watch for entries while even `bound` is missing. Daemons only.
    pub(crate) base: Option<PathBuf>,
}

/// The directories a namespace reads, by path.
pub(crate) type Needs = BTreeMap<PathBuf, Need>;

/// Record that `dir` is read for `name`, or for every `*.toml`.
pub(crate) fn need(needs: &mut Needs, dir: PathBuf, bound: &Path, name: Option<&OsStr>) {
    let entry = needs.entry(dir).or_insert_with(|| Need {
        bound: bound.to_path_buf(),
        names: Some(BTreeSet::new()),
        base: None,
    });
    match (name, &mut entry.names) {
        (Some(name), Some(names)) => {
            names.insert(name.to_os_string());
        }
        (None, names) => *names = None,
        (Some(_), None) => {}
    }
}

/// `needs`, plus the directory of each symlinked file's target, so edits through a link are seen.
pub(crate) fn followed(needs: &Needs) -> Needs {
    let mut all = needs.clone();
    for (dir, read) in needs {
        let names: Vec<OsString> = match &read.names {
            Some(names) => names.iter().cloned().collect(),
            None => std::fs::read_dir(dir)
                .into_iter()
                .flatten()
                .flatten()
                .map(|entry| entry.file_name())
                .filter(|name| paths::is_toml(name))
                .collect(),
        };
        for name in names {
            let path = dir.join(&name);
            // A link to `/dev/null` masks a file, so only regular files are followed.
            let file = std::fs::metadata(&path).is_ok_and(|meta| meta.is_file());
            if !file || !is_link(&path) {
                continue;
            }
            let Ok(target) = std::fs::canonicalize(&path) else {
                continue;
            };
            if let (Some(parent), Some(file)) = (target.parent(), target.file_name()) {
                need(&mut all, parent.to_path_buf(), parent, Some(file));
            }
        }
    }
    all
}

/// Whether `path` is a symlink.
fn is_link(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

/// How much of a namespace an event affects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Concern {
    None,
    /// A file it reads changed.
    Content,
    /// A directory it reads, or a parent of one, was created or removed.
    Structure,
}

/// How an event at `touched` affects a namespace that reads `needs`.
pub(crate) fn concern(needs: &Needs, touched: &Path) -> Concern {
    let one = |(dir, need): (&PathBuf, &Need)| {
        if dir.starts_with(touched) {
            return Concern::Structure;
        }
        let read = touched.parent() == Some(dir.as_path())
            && touched.file_name().is_some_and(|name| match &need.names {
                Some(names) => names.contains(name),
                None => paths::is_toml(name),
            });
        if !read {
            Concern::None
        } else if is_link(touched) {
            // A link may now point elsewhere, so its target needs watching.
            Concern::Structure
        } else {
            Concern::Content
        }
    };
    needs.iter().map(one).max().unwrap_or(Concern::None)
}

/// Each directory in `needs`, else its nearest existing ancestor, else its base.
pub(crate) fn targets(needs: &Needs) -> Targets {
    let mut targets = Targets::new();
    for (dir, need) in needs {
        let mut within = dir.ancestors().take_while(|a| a.starts_with(&need.bound));
        let target = match within.find(|a| a.is_dir()) {
            Some(found) => (found.to_path_buf(), Reach::Full),
            // A daemon waits on the base for the missing directory to appear.
            None => match &need.base {
                Some(base) if base.is_dir() => (base.clone(), Reach::Entries),
                _ => continue,
            },
        };
        merge(&mut targets, target);
    }
    targets
}

/// Add `target`, keeping the wider reach when a directory is already there.
pub(crate) fn merge(targets: &mut Targets, (dir, reach): (PathBuf, Reach)) {
    let had = targets.entry(dir).or_insert(reach);
    *had = (*had).max(reach);
}
