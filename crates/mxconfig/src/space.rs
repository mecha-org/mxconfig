//! A namespace held open by a [`crate::Source`].

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use crate::error::{Diagnostic, Error, Kind};
use crate::host::protocol::Failure;
use crate::lock;
use crate::migrate;
use crate::paths::{self, Home, Layer, LayerKind, Roots};
use crate::resolve::{self, Origin, Resolved};
use crate::settings::{Asset, Edit};
use crate::source::Detach;
use crate::transport::{Bus, Change, Served};
use crate::watch::{self, Concern, Needs, Targets, need};
use crate::wire::{Snapshot, Subscription};
use crate::write;
use mxconfig_schema::value::Value;
use mxconfig_schema::{AssetSchema, Class, Schema};

/// Shared by a space's handles. The space stays open while one exists.
#[derive(Debug)]
pub(crate) struct Key {
    pub(crate) namespace: String,
    /// The space's version lock, let go with the last handle.
    pub(crate) _held: Option<lock::Held>,
    /// Its subscription to the daemon, ended with the last handle or a detach.
    pub(crate) subscription: Mutex<Option<Subscription>>,
}

/// A file's inode, modification time and length, which any change alters.
type FileId = (u64, i64, i64, u64);

/// An asset's file path and [`FileId`].
type Stamp = Option<(PathBuf, Option<FileId>)>;

/// `path`'s [`FileId`], if it can be read.
fn file_id(path: &Path) -> Option<FileId> {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::metadata(path).ok()?;
    Some((meta.ino(), meta.mtime(), meta.mtime_nsec(), meta.len()))
}

pub(crate) struct Space {
    pub(crate) key: Weak<Key>,
    pub(crate) schema: Arc<Schema>,
    backing: Backing,
    pub(crate) resolved: Resolved,
    /// Each asset's file as last seen, to detect changes.
    assets: BTreeMap<String, Stamp>,
    /// Problems found while opening, migrating and writing.
    pub(crate) notes: Vec<Diagnostic>,
    /// Warnings for directories that could not be watched.
    pub(crate) unwatched: Vec<Diagnostic>,
    /// The diagnostics already reported as an outcome.
    pub(crate) reported: Vec<Diagnostic>,
    /// Why it stopped following its namespace, once it has. It then keeps its values.
    pub(crate) detached: Option<Detach>,
    /// Its version lock until a handle takes it.
    pub(crate) held: Option<lock::Held>,
    /// Its daemon subscription until a handle takes it.
    pub(crate) subscription: Option<Subscription>,
    /// The version that holds the namespace, when it isn't this one's. Writes are then refused.
    pub(crate) other: Option<u32>,
}

/// Where a space's values come from.
enum Backing {
    Files(Files),
    /// mxconfigd, with the asset files it has passed.
    Daemon(BTreeMap<String, Served>),
    /// The schema defaults, until the daemon serves the namespace.
    Defaults,
}

struct Files {
    layers: Vec<Layer>,
    /// The directories read and the entries that matter. Fixed at open.
    needs: Needs,
    /// `needs` plus each symlinked file's target, as of the last refresh.
    followed: Needs,
    /// The file this space last wrote, as it was then, so its own event is skipped.
    wrote: Option<(PathBuf, Option<FileId>)>,
    /// The file writes go to. `None` without a user dir.
    target: Option<PathBuf>,
    /// The file whose recorded version must match the schema's.
    versioned: Option<PathBuf>,
    /// The lock file writes and migrations hold, if this space writes.
    lock: Option<PathBuf>,
}

impl Space {
    /// `schema` over `roots`. The privileged daemon writes the admin layer.
    pub(crate) fn over(schema: Schema, roots: &Roots) -> Result<Space, Error> {
        let mut layers = roots.layers();
        let privileged = schema.class() == Class::Privileged;
        let bus = Bus::serving(schema.class());
        // Each daemon serves only its own namespaces.
        if roots.serving.is_some_and(|serving| serving != bus) {
            let why = format!("`{}` is served by {bus}", schema.namespace());
            return Err(Kind::Daemon(Failure::Unknown, why).into());
        }
        let mut notes = managed_notes(&schema, &layers);
        let (versioned, lock) = if privileged {
            // A privileged namespace is device-wide, so it has no user layer.
            layers.retain(|layer| layer.kind != LayerKind::User);
            let admin = layers.iter().find(|l| l.kind == LayerKind::Admin);
            // Only the privileged daemon writes it, so it needs no lock.
            (admin.map(|a| a.home(&schema).main_file()), None)
        } else {
            let user = layers.iter().find(|l| l.kind == LayerKind::User);
            let home = user.map(|user| user.home(&schema));
            let file = home.as_ref().map(Home::main_file);
            // Beside the home, not in it: an open file would keep a deleted home alive.
            let lock = home.map(|home| write::with_suffix(&home.dir, paths::LOCK_SUFFIX));
            (file, lock)
        };
        // Only the privileged daemon writes the admin file.
        let writes = !privileged || roots.serving == Some(Bus::System);
        let (held, other) = match lock.as_deref() {
            Some(path) => hold(&schema, path, &mut notes),
            None => (None, None),
        };
        // Another version holds it, so this one only reads, and only if its names read the same.
        if let Some(other) = other
            && !reads_across(&schema, other, versioned.as_deref())
        {
            let open = format!("the open `{}`", schema.namespace());
            return Err(Error::version_conflict(open, other, schema.version()));
        }
        let versioned = versioned.filter(|_| other.is_none());
        let target = versioned.clone().filter(|_| writes);
        // Create the user home now, so writes by other apps can be watched.
        for layer in layers.iter().filter(|l| l.kind == LayerKind::User) {
            let bound = layer.home(&schema).bound;
            if let Err(e) = paths::create_private_dir(&bound) {
                let bound = bound.display();
                notes.push(Diagnostic::warning(format!("cannot create {bound}: {e}")));
            }
        }
        if let Some(target) = &target {
            let migrate = || migrate::bring_up_to_date(target, &schema);
            let migrated = match (&lock, &held) {
                (Some(lock), Some(_)) => lock::exclusive(lock, migrate),
                // Migrating without the lock could clobber another version's file.
                (Some(_), None) if migrate::needs_migrating(target, &schema) => {
                    Err(unlocked(&schema, target))
                }
                // A lock that can't be held still lets writes go ahead.
                _ => migrate(),
            };
            notes.extend(migrated?);
        }
        // A reader can't migrate, and must not read a newer file as its own.
        let file = versioned.as_deref().and_then(migrate::file_version);
        if let Some(file) = file.filter(|&file| file > schema.version()) {
            return Err(moved_on(&schema, file));
        }
        let needs = needs_of(&schema, &layers);
        let files = Files {
            followed: needs.clone(),
            wrote: None,
            needs,
            layers,
            target,
            versioned,
            lock: lock.filter(|_| held.is_some()),
        };
        // Not read yet. The source adds watches first, then calls `read`.
        let space = Space::new(schema, Backing::Files(files), Resolved::empty(), 0);
        let mut space = Space {
            notes,
            held,
            other,
            ..space
        };
        if roots.serving.is_some() {
            space.wait_on_bases();
        }
        Ok(space)
    }

    /// Let a daemon see an admin home appear, by watching its base meanwhile.
    fn wait_on_bases(&mut self) {
        let Backing::Files(files) = &mut self.backing else {
            return;
        };
        for layer in files.layers.iter().filter(|l| l.kind == LayerKind::Admin) {
            let bound = layer.home(&self.schema).bound;
            let homes = files.needs.values_mut().filter(|need| need.bound == bound);
            homes.for_each(|need| need.base = Some(layer.root.clone()));
        }
    }

    /// A space backed by the daemon, read with `schema`, from its snapshot served at `other`.
    pub(crate) fn daemon(schema: Schema, snapshot: Snapshot, other: Option<u32>) -> Space {
        let resolved = Resolved::from_daemon(snapshot.state.values);
        let backing = Backing::Daemon(snapshot.assets);
        let space = Space::new(schema, backing, resolved, snapshot.state.revision);
        Space {
            subscription: Some(snapshot.subscription),
            other,
            ..space
        }
    }

    /// A shared namespace on its schema defaults, until its daemon serves it.
    pub(crate) fn defaults(schema: Schema) -> Space {
        let resolved = resolve::defaults(&schema);
        Space::new(schema, Backing::Defaults, resolved, 0)
    }

    /// Follow the daemon from now on, leaving the defaults.
    pub(crate) fn follow(&mut self) {
        if self.on_defaults() {
            self.backing = Backing::Daemon(BTreeMap::new());
        }
    }

    /// Replace its subscription, or end it with `None`.
    pub(crate) fn subscribe(&self, subscription: Option<Subscription>) {
        if let Some(key) = self.key.upgrade() {
            *key.subscription.lock().unwrap() = subscription;
        }
    }

    fn new(schema: Schema, backing: Backing, mut resolved: Resolved, revision: u64) -> Space {
        resolved.revision = revision;
        // `read` stamps the assets once the source has added the watches.
        Space {
            key: Weak::new(),
            schema: Arc::new(schema),
            backing,
            resolved,
            assets: BTreeMap::new(),
            notes: Vec::new(),
            unwatched: Vec::new(),
            reported: Vec::new(),
            detached: None,
            held: None,
            subscription: None,
            other: None,
        }
    }

    fn files_ref(&self) -> Option<&Files> {
        match &self.backing {
            Backing::Files(files) => Some(files),
            Backing::Daemon(_) | Backing::Defaults => None,
        }
    }

    pub(crate) fn is_daemon(&self) -> bool {
        matches!(self.backing, Backing::Daemon(_))
    }

    /// Whether it holds the schema defaults, waiting for its daemon.
    pub(crate) fn on_defaults(&self) -> bool {
        matches!(self.backing, Backing::Defaults)
    }

    /// The bus of the daemon this space follows or waits for.
    pub(crate) fn bus(&self) -> Option<Bus> {
        let waits = self.is_daemon() || self.on_defaults();
        waits.then(|| Bus::serving(self.schema.class()))
    }

    /// The bus of the daemon it follows, unless detached.
    pub(crate) fn following(&self) -> Option<Bus> {
        self.waits_on().filter(|_| self.is_daemon())
    }

    /// The bus of the daemon it follows or waits for, unless detached.
    pub(crate) fn waits_on(&self) -> Option<Bus> {
        self.bus().filter(|_| self.detached.is_none())
    }

    /// The bus `edits` go to, or `None` for local files.
    pub(crate) fn route(&self, edits: &[Edit]) -> Result<Option<Bus>, Error> {
        self.writable()?;
        // A managed key is only ever set by vendor and admin files.
        let managed = |edit: &&Edit| self.schema.key(edit.key()).is_some_and(|k| k.managed);
        if let Some(edit) = edits.iter().find(managed) {
            return Err(Kind::Managed(edit.key().to_string()).into());
        }
        if edits.is_empty() {
            return Ok(None);
        }
        // A space that follows a daemon writes through it.
        Ok(self.bus())
    }

    /// Its values, origins and every diagnostic, at its revision.
    pub(crate) fn state(&self) -> Resolved {
        Resolved {
            entries: self.resolved.entries.clone(),
            diagnostics: self.diagnostics(),
            revision: self.revision(),
        }
    }

    pub(crate) fn diagnostics(&self) -> Vec<Diagnostic> {
        let found = self.notes.iter().chain(&self.unwatched);
        let mut all: Vec<_> = found.chain(&self.resolved.diagnostics).cloned().collect();
        all.extend(self.asset_notes());
        if let Err(e) = self.attached() {
            all.push(Diagnostic::error(e.to_string()));
        }
        let namespace = self.schema.namespace();
        let version = self.schema.version();
        match (self.other, self.bus().filter(|_| self.on_defaults())) {
            (Some(other), Some(_)) => all.push(Diagnostic::error(format!(
                "`{namespace}` is at schema v{other}, and this is v{version}, so this handle keeps its defaults and only reads"
            ))),
            (Some(other), None) => all.push(Diagnostic::warning(format!(
                "`{namespace}` is held at schema v{other}, so this handle only reads"
            ))),
            (None, Some(bus)) => all.push(Diagnostic::warning(format!(
                "`{namespace}` holds its schema defaults until {bus} serves it"
            ))),
            (None, None) => {}
        }
        all
    }

    /// Warnings for asset files that are set but can't be read.
    fn asset_notes(&self) -> Vec<Diagnostic> {
        let Some(files) = self.files_ref() else {
            return Vec::new();
        };
        let mut notes = Vec::new();
        for decl in self.schema.assets() {
            if let Some(var) = decl.env_var()
                && let Some(value) = paths::env_var(var)
                && !Path::new(&value).is_file()
            {
                let why = format!("`{var}` names {value}, which is not a file, so it is ignored");
                notes.push(Diagnostic::warning(why));
            }
            for layer in &files.layers {
                let path = layer.home(&self.schema).dir.join(&decl.path);
                if path.is_dir() {
                    let why = format!("{} is a directory, so it is skipped", path.display());
                    notes.push(Diagnostic::warning(why));
                }
            }
        }
        notes
    }

    /// Fail if this space is detached.
    pub(crate) fn attached(&self) -> Result<(), Error> {
        if let Some(cause) = self.detached {
            return Err(Kind::Detached(self.schema.namespace().to_string(), cause).into());
        }
        Ok(())
    }

    /// Fail unless writes may go ahead: attached, and at the version holding the namespace.
    pub(crate) fn writable(&self) -> Result<(), Error> {
        self.attached()?;
        if let Some(other) = self.other {
            let what = format!("`{}`", self.schema.namespace());
            return Err(Error::version_conflict(what, other, self.schema.version()));
        }
        Ok(())
    }

    /// Whether a followed file moved to another version.
    pub(crate) fn moved(&self) -> bool {
        let Some(files) = self.files_ref().filter(|_| self.detached.is_none()) else {
            return false;
        };
        let own = self.schema.version();
        let file = files.versioned.as_deref().and_then(migrate::file_version);
        // A writer's file must match. A reader takes an older one as it is.
        let writes = files.target.is_some();
        file.is_some_and(|file| file > own || (writes && file < own))
    }

    /// Check `edits` against the schema without writing anything.
    pub(crate) fn validate(&self, edits: &[Edit]) -> Result<(), Error> {
        let mut set = edits
            .iter()
            .filter_map(|e| Some((e.key(), e.value.as_ref()?)));
        set.try_for_each(|(key, value)| validate_key(&self.schema, key, value))
    }

    /// Read local files for the first time. Reports no changes.
    pub(crate) fn read(&mut self) {
        let Backing::Files(files) = &self.backing else {
            return;
        };
        self.resolved = resolve::resolve(&self.schema, &files.layers);
        self.assets = self.stamps();
    }

    /// Re-read local files. Returns what changed.
    pub(crate) fn reload(&mut self) -> Vec<Change> {
        let Backing::Files(files) = &self.backing else {
            return Vec::new();
        };
        if self.detached.is_some() {
            return Vec::new();
        }
        let fresh = resolve::resolve(&self.schema, &files.layers);
        self.replace(fresh, self.revision() + 1)
    }

    /// Replace the values with `fresh` at `revision`. Returns what changed.
    pub(crate) fn replace(&mut self, mut fresh: Resolved, revision: u64) -> Vec<Change> {
        let changes = diff(&self.resolved, &fresh, self.schema.namespace(), revision);
        // Always take the daemon's revision, even with no changes.
        let moved = !changes.is_empty() || self.is_daemon();
        fresh.revision = if moved { revision } else { self.revision() };
        // Replace even when unchanged, to keep diagnostics current.
        self.resolved = fresh;
        changes
    }

    pub(crate) fn revision(&self) -> u64 {
        self.resolved.revision
    }

    /// Write `edits` to the local file, then re-read.
    pub(crate) fn apply(&mut self, edits: &[Edit]) -> Result<Vec<Change>, Error> {
        if edits.is_empty() {
            return Ok(Vec::new());
        }
        let target = self.target()?.to_path_buf();
        self.create_user_root()?;
        let aside = self.locked(|| {
            // A later schema version then knows to migrate this file.
            migrate::stamp(&target, &self.schema)?;
            write::edit(&target, |doc| {
                for edit in edits {
                    match &edit.value {
                        Some(value) => write::set_path(doc, edit.key(), write::to_item(value)),
                        None => {
                            write::remove_path(doc, edit.key());
                        }
                    }
                }
            })
        })?;
        self.note_aside(&target, aside);
        if let Backing::Files(files) = &mut self.backing {
            files.wrote = Some((target.clone(), file_id(&target)));
        }
        Ok(self.reload())
    }

    /// Remove keys the schema does not define. Returns the removed paths.
    pub(crate) fn prune(&mut self) -> Result<Vec<String>, Error> {
        self.writable()?;
        let target = self.target()?.to_path_buf();
        let pruned = self.locked(|| {
            let mut removed = migrate::prune_assets(&target, &self.schema)?;
            let Some(mut doc) = write::load_doc(&target) else {
                return Ok((removed, None));
            };
            let keys = migrate::prune(&mut doc, &self.schema);
            if keys.is_empty() {
                return Ok((removed, None));
            }
            removed.extend(keys);
            Ok((removed, write::write_doc(&target, &doc)?))
        });
        let (removed, aside) = pruned?;
        if !removed.is_empty() {
            self.note_aside(&target, aside);
            self.reload();
        }
        Ok(removed)
    }

    /// Run `body` while holding the write lock, if this space has one.
    fn locked<R>(&self, body: impl FnOnce() -> Result<R, Error>) -> Result<R, Error> {
        match self.files_ref().and_then(|files| files.lock.as_deref()) {
            Some(lock) => lock::exclusive(lock, body),
            None => body(),
        }
    }

    /// Add a note saying where a broken `target` was saved.
    fn note_aside(&mut self, target: &Path, aside: Option<PathBuf>) {
        if let Some(aside) = aside {
            self.notes.push(aside_note(target, &aside));
        }
    }

    /// The file for asset `id`, or `None` if nothing sets it.
    pub(crate) fn asset(&self, id: &str) -> Option<Asset> {
        let decl = self.schema.asset(id)?;
        let files = match &self.backing {
            Backing::Files(files) => files,
            Backing::Defaults => return None,
            Backing::Daemon(served) => {
                let served = served.get(id)?.clone();
                let path = served.path.clone();
                return Some(Asset {
                    served: Some(served),
                    ..asset(id, path, Origin::Daemon)
                });
            }
        };
        // An env var naming no file lets the layers apply, as for keys.
        if let Some(var) = decl.env_var()
            && let Some(value) = paths::env_var(var)
            && Path::new(&value).is_file()
        {
            let origin = Origin::Env { var: var.into() };
            return Some(asset(id, PathBuf::from(value), origin));
        }
        // A managed asset ignores the user's copy.
        let layers = files.layers.iter().rev();
        let mut layers = layers.filter(|layer| !(decl.managed && layer.kind == LayerKind::User));
        layers.find_map(|layer| {
            let path = layer.home(&self.schema).dir.join(&decl.path);
            let origin = Origin::File {
                kind: layer.kind,
                path: path.clone(),
            };
            path.is_file().then(|| asset(id, path, origin))
        })
    }

    /// Set the daemon's file for asset `id`. False if not a daemon asset.
    pub(crate) fn serve(&mut self, id: &str, file: Option<Served>) -> bool {
        let Backing::Daemon(served) = &mut self.backing else {
            return false;
        };
        if self.schema.asset(id).is_none() {
            return false;
        }
        match file {
            Some(file) => served.insert(id.to_string(), file),
            None => served.remove(id),
        };
        true
    }

    /// Replace the daemon's asset files. Returns the ids that may have changed.
    pub(crate) fn serve_all(&mut self, fresh: BTreeMap<String, Served>) -> Vec<String> {
        let Backing::Daemon(served) = &mut self.backing else {
            return Vec::new();
        };
        // New fds may have the same contents, so report them all.
        let ids: BTreeSet<String> = served.keys().chain(fresh.keys()).cloned().collect();
        *served = fresh;
        ids.into_iter().collect()
    }

    /// Write the user's copy of asset `id`, or remove it if `None`.
    pub(crate) fn put_asset(&self, id: &str, bytes: Option<&[u8]>) -> Result<(), Error> {
        let decl = self.asset_decl(id)?;
        let user = self.user_layer().ok_or_else(|| self.unwritable())?;
        let path = user.home(&self.schema).dir.join(&decl.path);
        let Some(bytes) = bytes else {
            return write::remove_if_present(&path).map_err(|e| Error::io(&path, e));
        };
        self.create_user_root()?;
        write::atomic_write(&path, bytes)
    }

    /// The schema's declaration of asset `id`.
    pub(crate) fn asset_decl(&self, id: &str) -> Result<&AssetSchema, Error> {
        let decl = self.schema.asset(id);
        decl.ok_or_else(|| {
            let namespace = self.schema.namespace().to_string();
            let id = id.to_string();
            Kind::UnknownAsset { namespace, id }.into()
        })
    }

    /// Update the asset stamps. Returns the ids whose file changed.
    pub(crate) fn moved_assets(&mut self) -> Vec<String> {
        let now = self.stamps();
        let moved = moved(&self.assets, &now);
        self.assets = now;
        moved
    }

    /// The directories to watch now, following symlinks again.
    pub(crate) fn watch_dirs(&mut self) -> Targets {
        let Backing::Files(files) = &mut self.backing else {
            return Targets::new();
        };
        files.followed = watch::followed(&files.needs);
        watch::targets(&files.followed)
    }

    /// How an event at `path` affects this space.
    pub(crate) fn concern(&self, path: &Path) -> Concern {
        let Some(files) = self.files_ref() else {
            return Concern::None;
        };
        // This space's own write was read already.
        let own = |(wrote, id): &(PathBuf, Option<FileId>)| {
            wrote == path && id.is_some() && file_id(path) == *id
        };
        if files.wrote.as_ref().is_some_and(own) {
            return Concern::None;
        }
        watch::concern(&files.followed, path)
    }

    /// Each asset's current file and metadata.
    fn stamps(&self) -> BTreeMap<String, Stamp> {
        // Daemon assets change only through the daemon's signals.
        if self.files_ref().is_none() {
            return BTreeMap::new();
        }
        let mut stamps = BTreeMap::new();
        for decl in self.schema.assets() {
            let stamp = self.asset(&decl.id).map(|asset| {
                let id = file_id(&asset.path);
                (asset.path, id)
            });
            stamps.insert(decl.id.clone(), stamp);
        }
        stamps
    }

    /// The file writes go to, or an error explaining why there is none.
    fn target(&self) -> Result<&Path, Error> {
        let target = self.files_ref().and_then(|files| files.target.as_deref());
        target.ok_or_else(|| self.unwritable())
    }

    fn user_layer(&self) -> Option<&Layer> {
        let files = self.files_ref()?;
        files.layers.iter().find(|l| l.kind == LayerKind::User)
    }

    /// Recreate the user home if it was removed. Returns a warning on failure.
    pub(crate) fn keep_home(&self) -> Option<Diagnostic> {
        let e = self.create_user_root().err()?;
        Some(Diagnostic::warning(e.to_string()))
    }

    /// Create the user home with mode 0700 if it is missing.
    fn create_user_root(&self) -> Result<(), Error> {
        let Some(user) = self.user_layer() else {
            return Ok(());
        };
        let bound = user.home(&self.schema).bound;
        paths::create_private_dir(&bound).map_err(|e| Error::io(&bound, e))
    }

    /// The error for a space with nowhere to write.
    fn unwritable(&self) -> Error {
        let namespace = self.schema.namespace().to_string();
        if self.schema.class() == Class::Privileged {
            let why =
                format!("`{namespace}` is privileged, so only the privileged service writes it");
            return Kind::Access(why).into();
        }
        Kind::NoUserDir(namespace).into()
    }
}

/// The ids in `now` whose stamp differs from `before`.
fn moved(before: &BTreeMap<String, Stamp>, now: &BTreeMap<String, Stamp>) -> Vec<String> {
    let moved = now
        .iter()
        .filter(|(id, stamp)| before.get(*id) != Some(stamp));
    moved.map(|(id, _)| id.clone()).collect()
}

/// An asset without a file passed by the daemon.
fn asset(id: &str, path: PathBuf, origin: Origin) -> Asset {
    Asset {
        id: id.to_string(),
        path,
        origin,
        served: None,
    }
}

/// The directories `schema` reads in `layers`, and the entries that matter.
fn needs_of(schema: &Schema, layers: &[Layer]) -> Needs {
    let mut needs = Needs::new();
    // The vendor layer only changes with OS updates, so it is not watched.
    for layer in layers.iter().filter(|l| l.kind != LayerKind::Vendor) {
        let home = layer.home(schema);
        let (main, drop_ins) = (home.main_file(), home.drop_in_dir());
        for name in [main.file_name(), drop_ins.file_name()] {
            need(&mut needs, home.dir.clone(), &home.bound, name);
        }
        need(&mut needs, drop_ins, &home.bound, None);
        // Watch each directory on the path to an asset.
        for asset in schema.assets() {
            let mut dir = home.dir.clone();
            for part in Path::new(&asset.path).components() {
                need(&mut needs, dir.clone(), &home.bound, Some(part.as_os_str()));
                dir.push(part);
            }
        }
    }
    // Watch the directory of an asset set by env var.
    let vars = schema.assets().iter().filter_map(|a| a.env_var());
    for path in vars.filter_map(paths::env_var).map(PathBuf::from) {
        if let (Some(dir), Some(name)) = (path.parent(), path.file_name()) {
            need(&mut needs, dir.to_path_buf(), dir, Some(name));
        }
    }
    needs
}

/// The warning for a broken file saved aside before being overwritten.
pub(crate) fn aside_note(target: &Path, aside: &Path) -> Diagnostic {
    let (target, aside) = (target.display(), aside.display());
    Diagnostic::warning(format!(
        "{target} did not parse; it is kept as {aside}, and its last good copy was used"
    ))
}

/// Warnings for system files the user can write, if they hold device values.
fn managed_notes(schema: &Schema, layers: &[Layer]) -> Vec<Diagnostic> {
    if !schema.has_managed() && schema.class() != Class::Privileged {
        return Vec::new();
    }
    let system = layers.iter().filter(|l| l.kind != LayerKind::User).cloned();
    let files = paths::sources(&system.collect::<Vec<_>>(), schema);
    let writable = files
        .into_iter()
        .filter(|(_, file)| paths::user_writable(file));
    let note = |(_, file): (LayerKind, PathBuf)| {
        let file = file.display();
        Diagnostic::warning(format!(
            "{file} is writable by the current user, who can set device values there"
        ))
    };
    writable.map(note).collect()
}

/// Hold `schema`'s version on `path`, or name the other version that holds it.
fn hold(
    schema: &Schema,
    path: &Path,
    notes: &mut Vec<Diagnostic>,
) -> (Option<lock::Held>, Option<u32>) {
    // The lock file sits beside the namespace's home, where only the user may enter.
    let made = path.parent().map_or(Ok(()), paths::create_private_dir);
    match made.and_then(|()| lock::hold(path, schema.version())) {
        Ok(lock::Hold::Held(held)) => (Some(held), None),
        // An unlockable file still opens, without keeping other versions out.
        Err(e) => {
            let path = path.display();
            let note = format!("cannot lock {path}, so other versions aren't kept out: {e}");
            notes.push(Diagnostic::warning(note));
            (None, None)
        }
        Ok(lock::Hold::Taken(other)) => (None, Some(other)),
    }
}

/// Whether `schema` reads a file another version holds the same, for every name it declares.
fn reads_across(schema: &Schema, other: u32, versioned: Option<&Path>) -> bool {
    if other < schema.version() {
        return schema.reads_through(other, schema.migrations());
    }
    // A newer version's steps are known only once it has recorded them.
    versioned.is_some_and(|file| schema.reads_through(other, &migrate::recorded(file)))
}

/// The error for a file that needs migrating while its lock can't be held.
fn unlocked(schema: &Schema, target: &Path) -> Error {
    let (namespace, target) = (schema.namespace(), target.display());
    let why = format!("`{namespace}` can't migrate {target}, as its version lock can't be held");
    Kind::Unlocked(why).into()
}

/// The error for a file a newer schema version has already migrated.
fn moved_on(schema: &Schema, file: u32) -> Error {
    let what = format!("`{}`'s file", schema.namespace());
    Error::version_conflict(what, file, schema.version())
}

/// Validate one key and value.
fn validate_key(schema: &Schema, path: &str, value: &Value) -> Result<(), Error> {
    let key = schema.key(path).ok_or_else(|| Kind::UnknownKey {
        namespace: schema.namespace().to_string(),
        key: path.to_string(),
    })?;
    match key.check(value).into_iter().next() {
        Some(reason) => Err(Error::invalid(path, reason)),
        None => Ok(()),
    }
}

/// The changes between two resolutions: changed values, then unset keys.
fn diff(old: &Resolved, new: &Resolved, namespace: &str, revision: u64) -> Vec<Change> {
    let change = |key: &String, value: Option<&Value>| Change {
        namespace: namespace.to_string(),
        key: key.clone(),
        value: value.cloned(),
        revision,
    };
    let set = new
        .iter()
        .filter(|(key, value)| old.get(key) != Some(*value))
        .map(|(key, value)| change(key, Some(value)));
    let unset = old
        .iter()
        .filter(|(key, _)| new.get(key).is_none())
        .map(|(key, _)| change(key, None));
    set.chain(unset).collect()
}
