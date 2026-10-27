//! The event source a runtime drives. Results are reported as outcomes.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::marker::PhantomData;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::error::{Diagnostic, Error, ErrorKind, Kind};
use crate::paths::Roots;
use crate::resolve::Resolved;
use crate::runtime::Drive;
use crate::settings::{Edit, Settings, Side, sides};
use crate::space::{Key, Space};
use crate::transport::{self, Bus, Change, Kept, Presence, Released, Served};
use crate::watch::{self, Concern, Targets, Watches};
use crate::wire;
use crate::wire::{Arg, Call, Heard, State};
use mxconfig_schema::Class;
use mxconfig_schema::Schema;

/// Message for lookups that can't fail: a handle keeps its space open.
const OPEN: &str = "a handle keeps its namespace open";

/// A type built from an open handle, such as a generated client.
pub trait FromSettings: Sized {
    /// Wrap the handle `settings`.
    fn from_settings(settings: Settings) -> Self;
}

impl FromSettings for Settings {
    fn from_settings(settings: Settings) -> Self {
        settings
    }
}

/// A generated client: its namespace, schema and typed changes.
pub trait Namespace: FromSettings {
    /// The namespace id, such as `org.example.player`.
    const NAMESPACE: &'static str;
    /// The schema it was generated from, which it reads with.
    const SCHEMA: &'static str;
    /// A typed value or asset change.
    type Change;

    /// A value change in this namespace, typed.
    fn change(change: &Change) -> Option<Self::Change>;

    /// A change to asset `id`, typed. A schema without assets has none.
    fn asset_change(_id: &str) -> Option<Self::Change> {
        None
    }
}

/// Something that happened in a [`Source`], untyped. Apps read it as an [`Event`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Outcome {
    /// `namespace` opened.
    Opened {
        ticket: TicketId,
        namespace: String,
        settings: Settings,
    },
    /// `namespace` didn't open.
    OpenFailed {
        ticket: TicketId,
        namespace: String,
        error: Error,
    },
    /// A write to `namespace` landed. Its changes arrive as `Changed`.
    Written { ticket: TicketId, namespace: String },
    /// A write to `namespace` was refused or may not have landed.
    WriteFailed {
        ticket: TicketId,
        namespace: String,
        error: Error,
    },
    /// A value changed.
    Changed(Change),
    /// An asset's file changed.
    AssetChanged { namespace: String, id: String },
    /// `namespace`'s handle stopped following it, and keeps its values.
    Detached { namespace: String, cause: Detach },
    /// `namespace` has problems it didn't have before: at open, or after a change. Log them.
    Diagnosed {
        namespace: String,
        diagnostics: Vec<Diagnostic>,
    },
}

impl Outcome {
    /// The namespace this outcome is about.
    pub fn namespace(&self) -> &str {
        match self {
            Outcome::Opened { namespace, .. }
            | Outcome::OpenFailed { namespace, .. }
            | Outcome::Written { namespace, .. }
            | Outcome::WriteFailed { namespace, .. }
            | Outcome::AssetChanged { namespace, .. }
            | Outcome::Detached { namespace, .. }
            | Outcome::Diagnosed { namespace, .. } => namespace,
            Outcome::Changed(change) => &change.namespace,
        }
    }

    /// This outcome as a typed change of `T`, if it is one.
    pub fn change<T: Namespace>(&self) -> Option<T::Change> {
        match self {
            Outcome::Changed(change) if change.namespace == T::NAMESPACE => T::change(change),
            Outcome::AssetChanged { namespace, id } if namespace == T::NAMESPACE => {
                T::asset_change(id)
            }
            _ => None,
        }
    }

    /// This outcome as an event of `T`'s namespace, if it is about it.
    pub fn event<T: Namespace>(&self) -> Option<Event<T>> {
        if self.namespace() != T::NAMESPACE {
            return None;
        }
        Some(match self {
            Outcome::Opened { settings, .. } => Event::Opened(T::from_settings(settings.clone())),
            Outcome::OpenFailed { error, .. } => Event::OpenFailed(error.clone()),
            Outcome::Written { .. } => Event::Written,
            Outcome::WriteFailed { error, .. } => Event::WriteFailed(error.clone()),
            Outcome::Detached { cause, .. } => Event::Detached(*cause),
            Outcome::Diagnosed { diagnostics, .. } => Event::Diagnosed(diagnostics.clone()),
            Outcome::Changed(_) | Outcome::AssetChanged { .. } => {
                Event::Changed(self.change::<T>()?)
            }
        })
    }
}

/// An outcome of one namespace, typed by its generated client `T`.
#[derive(Debug)]
#[non_exhaustive]
pub enum Event<T: Namespace> {
    /// The namespace opened.
    Opened(T),
    /// The namespace didn't open.
    OpenFailed(Error),
    /// A value or asset changed, by any writer.
    Changed(T::Change),
    /// A write landed. Its changes arrive as `Changed`.
    Written,
    /// A write was refused or may not have landed.
    WriteFailed(Error),
    /// The handle stopped following its namespace, and keeps its values.
    Detached(Detach),
    /// The namespace has problems it didn't have before. Log them.
    Diagnosed(Vec<Diagnostic>),
}

/// Why a handle stopped following its namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Detach {
    /// Its file moved to another schema version. A restart picks that up.
    Moved,
    /// Its transport was closed. Drop its handles and open it again.
    Closed,
}

impl std::fmt::Display for Detach {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Detach::Moved => "its file moved to another schema version, so restart to use it",
            Detach::Closed => "its transport was closed, so drop its handles and open it again",
        })
    }
}

/// The ticket type for writes.
#[derive(Debug)]
pub enum Write {}

/// The id of an open or a write, as its outcome carries it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TicketId(u64);

impl From<TicketId> for u64 {
    fn from(id: TicketId) -> u64 {
        id.0
    }
}

/// Identifies an open or a write until its outcome arrives.
pub struct Ticket<T> {
    id: TicketId,
    of: PhantomData<fn() -> T>,
}

impl<T> Ticket<T> {
    fn new(id: TicketId) -> Self {
        Ticket {
            id,
            of: PhantomData,
        }
    }

    /// The id its outcome carries.
    pub fn id(&self) -> TicketId {
        self.id
    }
}

impl<T> Clone for Ticket<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Ticket<T> {}

impl<T> PartialEq for Ticket<T> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl<T> Eq for Ticket<T> {}

impl<T> std::hash::Hash for Ticket<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl<T> std::fmt::Debug for Ticket<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Ticket({})", self.id.0)
    }
}

impl<T: FromSettings> Ticket<T> {
    /// The opened handle, if `outcome` is for this open.
    pub fn opened(&self, outcome: &Outcome) -> Option<Result<T, Error>> {
        match outcome {
            Outcome::Opened {
                ticket, settings, ..
            } if *ticket == self.id => Some(Ok(T::from_settings(settings.clone()))),
            Outcome::OpenFailed { ticket, error, .. } if *ticket == self.id => {
                Some(Err(error.clone()))
            }
            _ => None,
        }
    }
}

impl Ticket<Write> {
    /// The write's result, if `outcome` is for this write.
    pub fn written(&self, outcome: &Outcome) -> Option<Result<(), Error>> {
        match outcome {
            Outcome::Written { ticket, .. } if *ticket == self.id => Some(Ok(())),
            Outcome::WriteFailed { ticket, error, .. } if *ticket == self.id => {
                Some(Err(error.clone()))
            }
            _ => None,
        }
    }
}

/// The runtime-facing state. It never blocks, spawns threads or polls.
#[derive(Clone)]
pub struct Source {
    inner: Arc<Mutex<Inner>>,
}

impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Source").finish_non_exhaustive()
    }
}

#[derive(Default)]
struct Inner {
    roots: Roots,
    /// The newest state heard while a namespace's `Subscribe` is in flight.
    early: BTreeMap<String, State>,
    /// Kept descriptors dropped here, for their transports to close.
    released: Released,
    /// Created when the runtime first asks for the fd.
    watches: Option<Watches>,
    /// The directories the spaces need watched.
    wanted: Targets,
    /// Whether `wanted` changed since it was last taken.
    wanted_changed: bool,
    reach: BTreeSet<Bus>,
    next: u64,
    /// The open namespaces. Each is opened once and shared.
    spaces: BTreeMap<String, Space>,
    outbox: Vec<(Bus, Call)>,
    /// The last known owner of each daemon's bus name. `None` once it has none.
    owners: BTreeMap<Bus, Option<String>>,
    /// Requests in flight, with the bus each went to.
    pending: BTreeMap<u64, (Bus, Pending)>,
    outcomes: Vec<Outcome>,
    /// The schemas clients carry, which they read with.
    embedded: BTreeMap<String, String>,
    /// The app id set with [`Source::set_app`].
    app: Option<String>,
}

/// An open on its way through the backends.
struct Open {
    ticket: TicketId,
    namespace: String,
    /// The backends left to try, the next one first.
    sides: Vec<Side>,
    /// The first backend that failed, and why.
    failed: Option<(Side, Error)>,
}

/// What a request in flight is for.
enum Pending {
    /// Who owns the daemon's name.
    Owner,
    /// An open waiting for its snapshot.
    Subscribe(Open),
    /// A fresh snapshot for an open namespace, after a restart or a bad signal.
    Resubscribe { namespace: String },
    /// A write.
    Apply { ticket: TicketId, namespace: String },
    /// An asset write. Holds the file open until the daemon replies.
    WriteAsset {
        ticket: TicketId,
        namespace: String,
        _file: Option<Served>,
    },
}

impl Pending {
    /// Whether this is a write.
    fn writes(&self) -> bool {
        matches!(self, Pending::Apply { .. } | Pending::WriteAsset { .. })
    }
}

impl Default for Source {
    fn default() -> Self {
        Source::new()
    }
}

impl Source {
    /// A source over this system's roots.
    pub fn new() -> Source {
        Source::with_roots(Roots::find())
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap()
    }

    /// Read the open space for `namespace`.
    pub(crate) fn space<R>(&self, namespace: &str, read: impl FnOnce(&Space) -> R) -> R {
        read(self.lock().spaces.get(namespace).expect(OPEN))
    }

    /// Change the open space for `namespace`.
    pub(crate) fn space_mut<R>(&self, namespace: &str, change: impl FnOnce(&mut Space) -> R) -> R {
        change(self.lock().spaces.get_mut(namespace).expect(OPEN))
    }

    /// Open `T`'s namespace, from files or its daemon. [`Event::Opened`] or [`Event::OpenFailed`] answers it.
    pub fn request<T: Namespace>(&self) -> Ticket<T> {
        let mut inner = self.lock();
        let schema = T::SCHEMA.to_string();
        inner.embedded.insert(T::NAMESPACE.to_string(), schema);
        Ticket::new(self.begin(&mut inner, T::NAMESPACE))
    }

    /// Ask to open `namespace` untyped, from files or through the daemon.
    #[doc(hidden)]
    pub fn request_namespace(&self, namespace: &str) -> Ticket<Settings> {
        Ticket::new(self.begin(&mut self.lock(), namespace))
    }

    /// Start opening `namespace`. Returns its ticket.
    fn begin(&self, inner: &mut Inner, namespace: &str) -> TicketId {
        // A closed space must not be found. A reopen keeps its watches, as re-adding one is slow.
        inner.drop_closed();
        let ticket = inner.ticket();
        let mut open = Open {
            ticket,
            namespace: namespace.to_string(),
            sides: Vec::new(),
            failed: None,
        };
        if let Some(live) = self.existing(inner, namespace) {
            inner.finish(open, live);
            return ticket;
        }
        let schema = inner.schema(namespace).ok();
        match sides(namespace, schema.as_ref(), inner.roots.serving.is_some()) {
            Ok(sides) => {
                open.sides = sides;
                self.attempt(inner, open);
            }
            Err(e) => inner.finish(open, Err(e)),
        }
        ticket
    }

    /// Try `open` on its next backend.
    fn attempt(&self, inner: &mut Inner, mut open: Open) {
        let side = open.sides.remove(0);
        let result = match side {
            Side::Local => inner
                .schema(&open.namespace)
                .and_then(|schema| self.load(inner, schema)),
            Side::Daemon => {
                let bus = bus_of(&open.namespace).expect("sides checked the class");
                if !inner.reach.contains(&bus) {
                    return self.settle(inner, open, side, Err(Kind::Unreachable(bus).into()));
                }
                let (id, app) = (inner.id(), inner.app_for(&open.namespace));
                let schema = inner.embedded.get(&open.namespace).map(String::as_str);
                let call = Call::subscribe(id, &app, &open.namespace, schema);
                return inner.send(bus, call, Pending::Subscribe(open));
            }
        };
        self.settle(inner, open, side, result);
    }

    /// Finish `open` with `side`'s result, or fall back to its next backend.
    fn settle(
        &self,
        inner: &mut Inner,
        mut open: Open,
        side: Side,
        result: Result<Settings, Error>,
    ) {
        let result = match result {
            Ok(settings) => {
                if let Some((before, e)) = &open.failed {
                    let note = format!("{before} failed, so {side} is used: {e}");
                    let space = inner.spaces.get_mut(&open.namespace).expect(OPEN);
                    space.notes.push(Diagnostic::warning(note));
                }
                Ok(settings)
            }
            Err(e) if !open.sides.is_empty() && e.can_fall_back() => {
                open.failed.get_or_insert((side, e));
                return self.attempt(inner, open);
            }
            // A shared namespace its daemon can't serve at this version opens on its defaults.
            Err(e)
                if (e.unavailable() || e.kind() == ErrorKind::VersionConflict)
                    && inner.roots.serving.is_none()
                    && shared(&open.namespace) =>
            {
                match inner.schema(&open.namespace) {
                    Ok(schema) => {
                        let settings = self.attach(inner, Space::defaults(schema));
                        // Another version holds it, so the defaults must not be written over it.
                        let space = inner.spaces.get_mut(&open.namespace).expect(OPEN);
                        space.other = e.other_version();
                        Ok(settings)
                    }
                    Err(_) => Err(e),
                }
            }
            Err(then) => match open.failed.take() {
                Some((_, first)) => Err(Kind::Fallback { first, then }.into()),
                None => Err(then),
            },
        };
        inner.finish(open, result);
    }

    /// The open handle on `schema`'s namespace, or a new one over local files.
    fn load(&self, inner: &mut Inner, schema: Schema) -> Result<Settings, Error> {
        // A closed space must not be found, nor keep its version lock.
        inner.drop_closed();
        if let Some(open) = self.existing(inner, schema.namespace()) {
            return open;
        }
        let space = Space::over(schema, &inner.roots)?;
        Ok(self.attach(inner, space))
    }

    /// Add `space`, unless its namespace is already open.
    fn attach(&self, inner: &mut Inner, mut space: Space) -> Settings {
        let namespace = space.schema.namespace().to_string();
        if let Some(open) = self.live(inner, &namespace) {
            return open;
        }
        let key = Arc::new(Key {
            namespace: namespace.clone(),
            _held: space.held.take(),
            subscription: Mutex::new(space.subscription.take()),
        });
        space.key = Arc::downgrade(&key);
        inner.spaces.insert(namespace, space);
        // Watch before reading, so no write is missed.
        inner.refresh();
        inner
            .spaces
            .get_mut(&key.namespace)
            .expect("just added")
            .read();
        Settings::new(self.clone(), key)
    }

    /// The open handle on `namespace`, or why a detached one can't be opened again.
    fn existing(&self, inner: &Inner, namespace: &str) -> Option<Result<Settings, Error>> {
        let settings = self.live(inner, namespace)?;
        Some(inner.spaces[namespace].attached().map(|()| settings))
    }

    /// A handle on `namespace`, if one still exists.
    fn live(&self, inner: &Inner, namespace: &str) -> Option<Settings> {
        let key = inner.spaces.get(namespace)?.key.upgrade()?;
        Some(Settings::new(self.clone(), key))
    }

    /// Whether `bus` is carried and its daemon running.
    pub fn daemon(&self, bus: Bus) -> Presence {
        let inner = self.lock();
        if !inner.reach.contains(&bus) {
            Presence::NoBus
        } else if inner.owners.get(&bus).is_some_and(Option::is_some) {
            Presence::Running
        } else {
            Presence::NoDaemon
        }
    }

    /// Take the outcomes since the last call.
    pub fn outcomes(&self) -> Vec<Outcome> {
        std::mem::take(&mut self.lock().outcomes)
    }

    /// Forget `bus`: drop what wasn't sent, and fail what waits on it.
    fn drop_bus(&self, inner: &mut Inner, bus: Bus) {
        inner.reach.remove(&bus);
        let unsent: BTreeSet<u64> = inner
            .outbox
            .extract_if(.., |(to, _)| *to == bus)
            .map(|(_, call)| call.id)
            .collect();
        let stranded: Vec<_> = inner
            .pending
            .extract_if(.., |_, (to, _)| *to == bus)
            .collect();
        for (id, (_, pending)) in stranded {
            // A write already sent may have landed. One never sent did not.
            let e = if pending.writes() && !unsent.contains(&id) {
                Kind::Unconfirmed(bus)
            } else {
                Kind::Unreachable(bus)
            };
            self.answer(inner, bus, pending, Err(e.into()));
        }
    }

    /// Finish `pending` with its reply's arguments, or the error it got.
    fn answer(
        &self,
        inner: &mut Inner,
        bus: Bus,
        pending: Pending,
        result: Result<Vec<Arg>, Error>,
    ) {
        match pending {
            // A failed query means no daemon owns the name yet.
            Pending::Owner => {
                let owner = result.ok().and_then(wire::owner);
                inner.owner(bus, owner.unwrap_or_default());
            }
            Pending::Subscribe(open) => {
                let namespace = open.namespace.clone();
                let result = result.and_then(wire::snapshot).and_then(|snap| {
                    let (schema, other) = inner.read_with(&namespace, &snap.schema)?;
                    Ok(self.attach(inner, Space::daemon(schema, snap, other)))
                });
                self.settle(inner, open, Side::Daemon, result);
                inner.take_early(&namespace);
            }
            Pending::Resubscribe { namespace } => {
                inner.resubscribed(&namespace, result);
                inner.take_early(&namespace);
            }
            // The daemon sent `AssetChanged` before this reply.
            Pending::WriteAsset {
                ticket, namespace, ..
            } => inner.written(ticket, namespace, result.map(|_| ())),
            Pending::Apply { ticket, namespace } => {
                let state = match result.and_then(wire::applied) {
                    Ok(state) => state,
                    Err(e) => return inner.written(ticket, namespace, Err(e)),
                };
                // Newer than held only if a `Changed` was missed.
                inner.take(bus, &namespace, state);
                inner.written(ticket, namespace, Ok(()));
            }
        }
    }
}

impl Drive for Source {
    fn reach(&self, bus: Bus) {
        self.lock().reach(bus);
    }

    fn unreach(&self, bus: Bus) {
        self.drop_bus(&mut self.lock(), bus);
    }

    fn close(&self, bus: Bus) {
        let mut inner = self.lock();
        // Detach the handles that followed this bus's daemon.
        let open: Vec<String> = inner
            .spaces
            .iter()
            .filter(|(_, s)| s.following() == Some(bus))
            .map(|(n, _)| n.clone())
            .collect();
        for namespace in open {
            inner.detach(&namespace, Detach::Closed);
        }
        // A `Subscribe` still in flight ends its subscription when its reply is dropped.
        self.drop_bus(&mut inner, bus);
    }

    fn kept(&self, bus: Bus, id: u64) -> Arg {
        Arg::Kept(Kept::new(bus, id, Arc::clone(&self.lock().released)))
    }

    fn take_released(&self, bus: Bus) -> Vec<u64> {
        let released = Arc::clone(&self.lock().released);
        let mut released = released.lock().unwrap();
        let mine = released.extract_if(.., |(on, _)| *on == bus);
        mine.map(|(_, id)| id).collect()
    }

    fn inotify_fd(&self) -> Result<OwnedFd, Error> {
        let mut inner = self.lock();
        if inner.watches.is_none() {
            inner.watches = Some(Watches::new()?);
            // Re-read, in case anything was written before the watches.
            inner.reread(|_| Concern::Structure);
        }
        let watches = inner.watches.as_ref().expect("made above");
        let fd = watches.fd().try_clone_to_owned();
        fd.map_err(|e| Error::io("<inotify>", e))
    }

    fn take_watch_dirs(&self) -> Option<Vec<PathBuf>> {
        let mut inner = self.lock();
        // A closed space must not keep its watches.
        inner.prune();
        let changed = std::mem::take(&mut inner.wanted_changed);
        changed.then(|| inner.wanted.keys().cloned().collect())
    }

    fn feed_inotify(&self, bytes: &[u8]) {
        let mut guard = self.lock();
        let inner = &mut *guard;
        let Some(watches) = &mut inner.watches else {
            return;
        };
        let events = watches.decode(bytes);
        inner.reread(|space| {
            let concern = |event: &watch::Event| match event {
                watch::Event::Touched(path) => space.concern(path),
                watch::Event::Overflow => Concern::Structure,
            };
            events.iter().map(concern).max().unwrap_or(Concern::None)
        });
    }

    fn touched(&self, path: &Path) {
        self.lock().reread(|space| space.concern(path));
    }

    fn rescan(&self) {
        self.lock().reread(|_| Concern::Structure);
    }

    fn take_calls(&self, bus: Bus) -> Vec<Call> {
        let mut inner = self.lock();
        inner.prune();
        inner
            .outbox
            .extract_if(.., |(to, _)| *to == bus)
            .map(|(_, call)| call)
            .collect()
    }

    fn deliver(&self, bus: Bus, event: transport::Event) {
        let mut inner = self.lock();
        let (id, result) = match event {
            transport::Event::Reply { id, args } => (id, Ok(args)),
            transport::Event::Failed { id, name, message } => (id, Err((name, message))),
            transport::Event::Signal {
                sender,
                interface,
                member,
                args,
            } => {
                if let Some(heard) = wire::heard(&interface, &member, args) {
                    inner.heard(bus, &sender, heard);
                }
                return;
            }
        };
        let Some((bus, pending)) = inner.pending.remove(&id) else {
            return;
        };
        let write = pending.writes();
        let result = result.map_err(|(name, message)| wire::failure(bus, &name, message, write));
        self.answer(&mut inner, bus, pending, result);
    }
}

impl Source {
    /// A source over `roots`.
    #[doc(hidden)]
    pub fn with_roots(roots: Roots) -> Self {
        let inner = Inner {
            roots,
            wanted_changed: true,
            ..Inner::default()
        };
        Source {
            inner: Arc::new(Mutex::new(inner)),
        }
    }

    /// Open `schema` from local files now, for a caller that holds its schema.
    #[doc(hidden)]
    pub fn open_schema(&self, schema: Schema) -> Result<Settings, Error> {
        self.load(&mut self.lock(), schema)
    }

    /// Carry `schema` for `namespace`: the one this client was built with.
    #[doc(hidden)]
    pub fn embed(&self, namespace: &str, schema: &str) {
        let mut inner = self.lock();
        inner.embedded.insert(namespace.into(), schema.into());
    }

    /// Name the app the daemon checks manifests for, such as `org.example.player`.
    ///
    /// Needed only by an app that reads shared settings and does not carry
    /// exactly one app namespace of its own. A Flatpak is always its own id.
    pub fn set_app(&self, app: &str) {
        self.lock().app = Some(app.to_string());
    }
}

impl Inner {
    /// Mark `bus` reachable, and ask who owns its daemon's name.
    fn reach(&mut self, bus: Bus) {
        if self.reach.insert(bus) {
            let call = Call::owner(self.id());
            self.send(bus, call, Pending::Owner);
            self.resubscribe_all(bus);
        }
    }

    fn id(&mut self) -> u64 {
        self.next += 1;
        self.next
    }

    fn ticket(&mut self) -> TicketId {
        TicketId(self.id())
    }

    /// Give `open`'s result as an outcome, then any problems it opened with.
    fn finish(&mut self, open: Open, result: Result<Settings, Error>) {
        let Open {
            ticket, namespace, ..
        } = open;
        let opened = result.is_ok();
        self.outcomes.push(match result {
            Ok(settings) => Outcome::Opened {
                ticket,
                namespace: namespace.clone(),
                settings,
            },
            Err(error) => Outcome::OpenFailed {
                ticket,
                namespace: namespace.clone(),
                error,
            },
        });
        // The problems go after the open they belong to.
        if opened {
            self.diagnose(&namespace);
        }
    }

    /// Report `namespace`'s diagnostics that weren't reported before.
    fn diagnose(&mut self, namespace: &str) {
        let Some(space) = self.spaces.get_mut(namespace) else {
            return;
        };
        let now = space.diagnostics();
        let fresh: Vec<_> = now
            .iter()
            .filter(|d| !space.reported.contains(d))
            .cloned()
            .collect();
        space.reported = now;
        if !fresh.is_empty() {
            let namespace = namespace.to_string();
            let diagnostics = fresh;
            self.outcomes.push(Outcome::Diagnosed {
                namespace,
                diagnostics,
            });
        }
    }

    fn written(&mut self, ticket: TicketId, namespace: String, result: Result<(), Error>) {
        self.outcomes.push(match result {
            Ok(()) => Outcome::Written { ticket, namespace },
            Err(error) => Outcome::WriteFailed {
                ticket,
                namespace,
                error,
            },
        });
    }

    /// Report `changes` to `namespace`, then the ids of its assets that `moved`.
    fn report(&mut self, namespace: &str, changes: Vec<Change>, moved: Vec<String>) {
        self.outcomes
            .extend(changes.into_iter().map(Outcome::Changed));
        self.outcomes
            .extend(moved.into_iter().map(|id| Outcome::AssetChanged {
                namespace: namespace.to_string(),
                id,
            }));
        self.diagnose(namespace);
    }

    /// Queue `call` for `bus` and track its reply.
    fn send(&mut self, bus: Bus, call: Call, pending: Pending) {
        self.pending.insert(call.id, (bus, pending));
        self.outbox.push((bus, call));
    }

    /// Send write `ticket` to `namespace` as `call` on `bus`, or fail it if the bus isn't reached.
    fn send_write(
        &mut self,
        bus: Bus,
        ticket: TicketId,
        namespace: &str,
        call: Call,
        pending: Pending,
    ) {
        if self.reach.contains(&bus) {
            self.send(bus, call, pending);
        } else {
            let namespace = namespace.to_string();
            self.written(ticket, namespace, Err(Kind::Unreachable(bus).into()));
        }
    }

    /// The app a call about `namespace` comes from. Empty if unknown.
    fn app_for(&self, namespace: &str) -> String {
        let mut own = self.embedded.keys().filter(|ns| Class::is_app(ns));
        // A namespace the app carries is its own; else the set id, else its only own one.
        let claimed = if own.clone().any(|ns| ns == namespace) {
            Some(namespace)
        } else if let Some(app) = &self.app {
            Some(app.as_str())
        } else {
            own.next()
                .filter(|_| own.next().is_none())
                .map(String::as_str)
        };
        claimed.unwrap_or_default().to_string()
    }

    /// `namespace`'s schema: the one the client carries, else the installed one.
    fn schema(&self, namespace: &str) -> Result<Schema, Error> {
        match self.embedded.get(namespace) {
            Some(text) => Ok(Schema::parse(text)?),
            None => self.roots.schema(namespace),
        }
    }

    /// Handle a decoded signal from `bus`.
    fn heard(&mut self, bus: Bus, sender: &str, heard: Heard) {
        // Take signals only from the daemon, about its own namespaces.
        let daemon = self.is_daemon(bus, sender);
        match heard {
            Heard::Owner(owner) => self.owner(bus, owner),
            _ if !daemon => {}
            Heard::Changed { namespace, .. } | Heard::AssetChanged { namespace, .. }
                if bus_of(&namespace) != Some(bus) => {}
            // The reply for a subscription in flight may be older than this.
            Heard::Changed { namespace, state } if self.subscribing(&namespace) => {
                let newer = |held: &State| held.revision < state.revision;
                if self.early.get(&namespace).is_none_or(newer) {
                    self.early.insert(namespace, state);
                }
            }
            Heard::Changed { namespace, state } => self.take(bus, &namespace, state),
            Heard::AssetChanged {
                namespace,
                id,
                file,
            } => {
                let space = self.spaces.get_mut(&namespace);
                if space.is_some_and(|space| space.serve(&id, file)) {
                    self.outcomes.push(Outcome::AssetChanged { namespace, id });
                }
            }
            Heard::Unreadable { namespace } => match namespace {
                Some(namespace) => self.resubscribe(&namespace),
                None => self.resubscribe_all(bus),
            },
        }
    }

    /// The daemon's bus name has `owner`. Empty when it has none.
    fn owner(&mut self, bus: Bus, owner: String) {
        let now = (!owner.is_empty()).then_some(owner);
        // A new owner knows nothing of our subscriptions.
        let restarted = now.is_some() && self.owners.get(&bus).is_some_and(|before| *before != now);
        self.owners.insert(bus, now);
        if restarted {
            self.restarted(bus);
        }
    }

    /// Whether `sender` owns the daemon's bus name on `bus`.
    fn is_daemon(&self, bus: Bus, sender: &str) -> bool {
        self.owners
            .get(&bus)
            .is_some_and(|owner| owner.as_deref() == Some(sender))
    }

    /// Ask for a fresh snapshot of `namespace`, if it is open through the daemon.
    fn resubscribe(&mut self, namespace: &str) {
        let Some(bus) = self
            .spaces
            .get(namespace)
            .and_then(Space::waits_on)
            .filter(|bus| self.reach.contains(bus))
        else {
            return;
        };
        let in_flight = self.pending.values().any(|(_, pending)| {
            matches!(pending, Pending::Resubscribe { namespace: n } if n == namespace)
        });
        if in_flight {
            return;
        }
        // Send the schema again, as a restarted daemon has forgotten it.
        let (id, app) = (self.id(), self.app_for(namespace));
        let schema = self.embedded.get(namespace).map(String::as_str);
        let call = Call::subscribe(id, &app, namespace, schema);
        let namespace = namespace.to_string();
        self.send(bus, call, Pending::Resubscribe { namespace });
    }

    /// Resubscribe after `bus`'s daemon restarted.
    fn restarted(&mut self, bus: Bus) {
        let sent = |to: &Bus, pending: &Pending| {
            *to == bus && matches!(pending, Pending::Resubscribe { .. })
        };
        self.pending.retain(|_, (to, pending)| !sent(to, pending));
        self.resubscribe_all(bus);
    }

    /// Resubscribe every namespace that follows or waits for `bus`'s daemon.
    fn resubscribe_all(&mut self, bus: Bus) {
        let remote: Vec<String> = self
            .spaces
            .iter()
            .filter(|(_, space)| space.bus() == Some(bus))
            .map(|(namespace, _)| namespace.clone())
            .collect();
        for namespace in remote {
            self.resubscribe(&namespace);
        }
    }

    /// The schema to read what the daemon serves with, and the served version if it is another.
    fn read_with(&self, namespace: &str, served: &str) -> Result<(Schema, Option<u32>), Error> {
        let served = Schema::parse_for(namespace, served)?;
        // A client that carries a schema uses it. Others read with the daemon's.
        let Some(own) = self.embedded.get(namespace) else {
            return Ok((served, None));
        };
        let own = Schema::parse(own)?;
        if own.version() == served.version() {
            return Ok((own, None));
        }
        // Another version reads on only where its names mean the same.
        if own.reads_same(&served) {
            return Ok((own, Some(served.version())));
        }
        let daemon = bus_of(namespace).unwrap_or(Bus::Session);
        let what = format!("`{namespace}` from {daemon}");
        Err(Error::version_conflict(
            what,
            served.version(),
            own.version(),
        ))
    }

    /// Take a resubscribe reply as `namespace`'s whole state, whatever its revision.
    fn resubscribed(&mut self, namespace: &str, result: Result<Vec<Arg>, Error>) {
        if self
            .spaces
            .get(namespace)
            .and_then(Space::waits_on)
            .is_none()
        {
            return;
        }
        let snap = result.and_then(wire::snapshot);
        let snap = snap.and_then(|snap| {
            let (_, other) = self.read_with(namespace, &snap.schema)?;
            Ok((other, snap))
        });
        let space = self.spaces.get_mut(namespace).expect("checked above");
        let (other, snap) = match snap {
            Ok(read) => read,
            // The defaults stay, and only a refusal is worth a diagnostic.
            Err(e) if space.on_defaults() => {
                // Another version holds it, so the defaults must not be written over it.
                space.other = e.other_version();
                if !e.unavailable() && space.other.is_none() {
                    let message = format!("could not subscribe, so the defaults stay: {e}");
                    space.resolved.diagnostics.push(Diagnostic::error(message));
                }
                return self.diagnose(namespace);
            }
            // Another version took over, so keep the values and stop following.
            Err(e) if e.kind() == ErrorKind::VersionConflict => {
                return self.detach(namespace, Detach::Moved);
            }
            Err(e) => {
                // Keep the old values, but say they may be stale.
                let message = format!("could not resubscribe, so values may be stale: {e}");
                space.resolved.diagnostics.push(Diagnostic::error(message));
                return self.diagnose(namespace);
            }
        };
        space.follow();
        space.other = other;
        space.subscribe(Some(snap.subscription));
        let (state, assets) = (snap.state, snap.assets);
        let changes = space.replace(Resolved::from_daemon(state.values), state.revision);
        let moved = space.serve_all(assets);
        self.report(namespace, changes, moved);
    }

    /// Stop `namespace` following its backing, and say why.
    fn detach(&mut self, namespace: &str, cause: Detach) {
        let space = self.spaces.get_mut(namespace).expect(OPEN);
        space.detached = Some(cause);
        space.subscribe(None);
        self.outcomes.push(Outcome::Detached {
            namespace: namespace.to_string(),
            cause,
        });
        // The problems go after the detach they come from.
        self.diagnose(namespace);
    }

    /// Whether a `Subscribe` for `namespace` is in flight.
    fn subscribing(&self, namespace: &str) -> bool {
        self.pending.values().any(|(_, pending)| match pending {
            Pending::Subscribe(open) => open.namespace == namespace,
            Pending::Resubscribe { namespace: n } => n == namespace,
            _ => false,
        })
    }

    /// Take a state that arrived before `namespace`'s subscription, if newer.
    fn take_early(&mut self, namespace: &str) {
        if self.subscribing(namespace) {
            return;
        }
        if let (Some(state), Some(bus)) = (self.early.remove(namespace), bus_of(namespace)) {
            self.take(bus, namespace, state);
        }
    }

    /// Take `state` from `bus` for `namespace` if it is newer.
    fn take(&mut self, bus: Bus, namespace: &str, state: State) {
        let Some(space) = self
            .spaces
            .get_mut(namespace)
            .filter(|s| s.following() == Some(bus))
        else {
            return;
        };
        if state.revision <= space.revision() {
            return;
        }
        let changes = space.replace(Resolved::from_daemon(state.values), state.revision);
        self.report(namespace, changes, Vec::new());
    }

    /// Re-read the affected namespaces, updating watches first if needed.
    fn reread(&mut self, concern: impl Fn(&Space) -> Concern) {
        self.prune();
        let picked: Vec<(String, Concern)> = self
            .spaces
            .iter()
            .map(|(namespace, space)| (namespace.clone(), concern(space)))
            .filter(|(_, concern)| *concern != Concern::None)
            .collect();
        // Update the watches before reading, so no write is missed.
        if picked.iter().any(|(_, c)| *c >= Concern::Structure) {
            self.refresh();
        }
        for (namespace, _) in picked {
            let space = self.spaces.get_mut(&namespace).expect("picked above");
            if space.moved() {
                self.detach(&namespace, Detach::Moved);
                continue;
            }
            let changes = space.reload();
            let moved = space.moved_assets();
            self.report(&namespace, changes, moved);
        }
    }

    /// Update the watches to match what the spaces read.
    fn refresh(&mut self) {
        // Recreate a removed user home, so it stays watched.
        for space in self.spaces.values_mut() {
            space.unwatched = space.keep_home().into_iter().collect();
        }
        let dirs: Vec<_> = self.spaces.values_mut().map(Space::watch_dirs).collect();
        let mut wanted = Targets::new();
        dirs.iter()
            .flatten()
            .for_each(|(d, &r)| watch::merge(&mut wanted, (d.clone(), r)));
        self.wanted_changed |= wanted != self.wanted;
        self.wanted = wanted;
        let Some(watches) = &mut self.watches else {
            return;
        };
        watches.retain(&self.wanted);
        for (space, dirs) in self.spaces.values_mut().zip(&dirs) {
            space.unwatched.extend(watches.add(dirs));
        }
    }

    /// Drop spaces without handles. Their subscriptions ended with the handles.
    fn prune(&mut self) {
        if self.drop_closed() {
            self.refresh();
        }
    }

    /// Drop spaces without handles, leaving their watches to the next refresh. Returns whether any went.
    fn drop_closed(&mut self) -> bool {
        let before = self.spaces.len();
        self.spaces.retain(|_, space| space.key.strong_count() > 0);
        self.spaces.len() != before
    }
}

impl Settings {
    /// Check `edits` as [`Settings::write`] would, without writing anything.
    #[doc(hidden)]
    pub fn check(&self, edits: &[Edit]) -> Result<(), Error> {
        self.plan(&self.source().lock(), edits).map(|_| ())
    }

    /// Where `edits` go if they may be written: a bus, or `None` for files.
    fn plan(&self, inner: &Inner, edits: &[Edit]) -> Result<Option<Bus>, Error> {
        let space = inner.spaces.get(self.namespace()).expect(OPEN);
        space.validate(edits)?;
        space.route(edits)
    }

    /// Write `edits`. Validated now, answered by [`Event::Written`] or [`Event::WriteFailed`].
    pub fn write(&self, edits: &[Edit]) -> Result<Ticket<Write>, Error> {
        let mut guard = self.source().lock();
        let inner = &mut *guard;
        let namespace = self.namespace();
        let route = self.plan(inner, edits)?;
        let ticket = inner.ticket();
        match route {
            None => {
                let space = inner.spaces.get_mut(namespace).expect(OPEN);
                let result = space.apply(edits);
                let result = result.map(|changes| inner.report(namespace, changes, Vec::new()));
                inner.written(ticket, namespace.to_string(), result);
            }
            Some(bus) => {
                let app = inner.app_for(namespace);
                let call = Call::apply(inner.id(), &app, namespace, edits);
                let pending = Pending::Apply {
                    ticket,
                    namespace: namespace.to_string(),
                };
                inner.send_write(bus, ticket, namespace, call, pending);
            }
        }
        Ok(Ticket::new(ticket))
    }

    /// Write `bytes` as the user's copy of asset `id`.
    pub fn write_asset(&self, id: &str, bytes: &[u8]) -> Result<Ticket<Write>, Error> {
        self.put_asset(id, Some(Content::Bytes(bytes)))
    }

    /// Copy the regular file `file` as the user's copy of asset `id`.
    pub fn write_asset_from(&self, id: &str, file: OwnedFd) -> Result<Ticket<Write>, Error> {
        self.put_asset(id, Some(Content::File(file)))
    }

    /// Remove the user's copy of asset `id`.
    pub fn clear_asset(&self, id: &str) -> Result<Ticket<Write>, Error> {
        self.put_asset(id, None)
    }

    /// Check that asset `id` may be written, as the asset writes would.
    #[doc(hidden)]
    pub fn check_asset(&self, id: &str) -> Result<(), Error> {
        self.asset_allowed(&self.source().lock(), id)
    }

    /// Whether asset `id` may be written at all.
    fn asset_allowed(&self, inner: &Inner, id: &str) -> Result<(), Error> {
        let space = inner.spaces.get(self.namespace()).expect(OPEN);
        space.writable()?;
        if space.asset_decl(id)?.managed {
            return Err(Kind::Managed(id.to_string()).into());
        }
        // A privileged namespace's assets come from its system files only.
        if space.schema.class() == Class::Privileged {
            let namespace = self.namespace();
            let why = format!("`{namespace}` is privileged, so its assets are installed files");
            return Err(Kind::Access(why).into());
        }
        Ok(())
    }

    /// Write or clear asset `id`.
    fn put_asset(&self, id: &str, content: Option<Content<'_>>) -> Result<Ticket<Write>, Error> {
        let mut guard = self.source().lock();
        let inner = &mut *guard;
        self.asset_allowed(inner, id)?;
        let namespace = self.namespace();
        let bus = inner.spaces.get(namespace).expect(OPEN).bus();
        let ticket = inner.ticket();
        if let Some(bus) = bus {
            // The daemon takes a file, so put bytes in one first.
            let file = content
                .map(Content::into_served)
                .transpose()
                .map_err(|e| Error::io("<asset>", e))?;
            let app = inner.app_for(namespace);
            let call = Call::write_asset(inner.id(), &app, namespace, id, file.as_ref());
            let pending = Pending::WriteAsset {
                ticket,
                namespace: namespace.to_string(),
                _file: file,
            };
            inner.send_write(bus, ticket, namespace, call, pending);
        } else {
            let space = inner.spaces.get_mut(namespace).expect(OPEN);
            let result = content
                .map(Content::into_bytes)
                .transpose()
                .map_err(|e| Error::io("<asset>", e))
                .and_then(|bytes| space.put_asset(id, bytes.as_deref()));
            let moved = space.moved_assets();
            inner.report(namespace, Vec::new(), moved);
            inner.written(ticket, namespace.to_string(), result);
        }
        Ok(Ticket::new(ticket))
    }
}

/// The content of an asset write: bytes or a file.
enum Content<'a> {
    Bytes(&'a [u8]),
    File(OwnedFd),
}

impl<'a> Content<'a> {
    fn into_bytes(self) -> std::io::Result<Cow<'a, [u8]>> {
        match self {
            Content::Bytes(bytes) => Ok(Cow::Borrowed(bytes)),
            Content::File(fd) => Served::new(fd).read().map(Cow::Owned),
        }
    }

    fn into_served(self) -> std::io::Result<Served> {
        match self {
            Content::Bytes(bytes) => Served::from_bytes(bytes),
            Content::File(fd) => Ok(Served::new(fd)),
        }
    }
}

/// Whether `namespace` is a valid shared namespace.
fn shared(namespace: &str) -> bool {
    Class::of(namespace).is_ok_and(|class| class != Class::App)
}

/// The bus of `namespace`'s daemon, if the id is valid.
fn bus_of(namespace: &str) -> Option<Bus> {
    Class::of(namespace).ok().map(Bus::serving)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::READS;

    const SCHEMA: &str = "namespace = \"org.mechanix.demo\"\nversion = 1\n";

    fn reads() -> usize {
        READS.with(std::cell::Cell::get)
    }

    /// An open reads its files once, whenever the watches start.
    #[test]
    fn an_open_reads_its_files_once() {
        let tmp = tempfile::tempdir().unwrap();
        let source = Source::with_roots(Roots::under(tmp.path()));
        let _fd = source.inotify_fd().unwrap();
        let before = reads();
        let _settings = source.open_schema(Schema::parse(SCHEMA).unwrap()).unwrap();
        assert_eq!(reads() - before, 1, "watched first, so one read");

        // Without watches, creating the fd causes one more read.
        let other = Source::with_roots(Roots::under(tmp.path()));
        let before = reads();
        let _settings = other.open_schema(Schema::parse(SCHEMA).unwrap()).unwrap();
        assert_eq!(reads() - before, 1);
        let _fd = other.inotify_fd().unwrap();
        assert_eq!(reads() - before, 2);
        let _again = other.inotify_fd().unwrap();
        assert_eq!(reads() - before, 2, "one watch set, made once");
    }

    /// Each call names the app it is from, as the daemon checks manifests by it.
    #[test]
    fn a_call_comes_from_the_app_it_carries_or_the_one_set() {
        let (shared, keyboard, notes) = (
            "org.mechanix.desktop.display",
            "org.mechanix.keyboard",
            "org.mechanix.notifications",
        );
        let source = Source::new();
        let app_for = |namespace| source.lock().app_for(namespace);
        assert_eq!(app_for(shared), "", "nothing known");

        // One app namespace carried is the app. A shared schema carried doesn't count.
        source.embed(shared, "");
        source.embed(keyboard, "");
        assert_eq!(app_for(shared), keyboard);

        // With two, shared calls need the app set. Each own namespace stays its own.
        source.embed(notes, "");
        assert_eq!(app_for(shared), "");
        source.set_app("org.mechanix.launcher");
        assert_eq!(app_for(shared), "org.mechanix.launcher");
        assert_eq!(app_for(keyboard), keyboard);
        assert_eq!(app_for(notes), notes);
    }
}
