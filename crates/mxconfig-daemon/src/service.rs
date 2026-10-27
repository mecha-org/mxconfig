//! The D-Bus service.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::{self, Read, Seek};
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use mxconfig::host::protocol::WRITE_ACTION;
use mxconfig::host::protocol::{self, BUS_NAME, Entry, Failure, MAX_ASSET, OBJECT_PATH};
use mxconfig::host::{Roots, Sandbox};
use mxconfig::runtime::Drive;
use mxconfig::{Access, Bus, Class, Edit, Outcome, Schema, Settings, Source, Ticket, Write};
use tokio::io::unix::AsyncFd;
use zbus::fdo;
use zbus::message::Header;
use zbus::names::{BusName, ErrorName, OwnedBusName};
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedFd as ZFd, Value as ZValue};

use crate::policy::{self, Caller, grants};

/// Settings on the wire: key path to a variant of TOML text.
type ValueMap = BTreeMap<String, ZValue<'static>>;

/// An asset file on the wire: its path and a read-only fd.
type AssetFile = (String, ZFd);

/// The polkit flag that lets it ask the user to authenticate.
const ALLOW_USER_INTERACTION: u32 = 1;

/// Polkit's bus name.
const POLKIT: &str = "org.freedesktop.PolicyKit1";

/// The object path of polkit's authority.
const POLKIT_PATH: &str = "/org/freedesktop/PolicyKit1/Authority";

/// The interface of polkit's authority.
const POLKIT_AUTHORITY: &str = "org.freedesktop.PolicyKit1.Authority";

/// The authority's method that checks one action.
const CHECK_AUTHORIZATION: &str = "CheckAuthorization";

/// The polkit subject kind for a caller on the bus.
const BUS_NAME_SUBJECT: &str = "system-bus-name";

/// The polkit detail that names the namespace being written.
const NAMESPACE_DETAIL: &str = "namespace";

/// Bytes read from the inotify fd at once.
const INOTIFY_BUFFER: usize = 4096;

/// The callers remembered at once. No signal is followed to forget them, as one can't be lost.
const MAX_CLIENTS: usize = 1024;

/// A wire entry as an edit. Text that is not one TOML value is refused.
fn entry_edit(entry: Entry) -> Reply<Edit> {
    let (key, value) =
        protocol::decode(entry).map_err(|e| fail(Failure::Invalid, e.to_string()))?;
    Ok(match value {
        Some(value) => Edit::set(key, value),
        None => Edit::remove(key),
    })
}

/// mxconfigd's error: a [`Failure`], under the name clients read, and why.
#[derive(Debug)]
struct Refusal(Failure, String);

impl zbus::DBusError for Refusal {
    fn create_reply(&self, call: &Header<'_>) -> zbus::Result<zbus::Message> {
        zbus::Message::error(call, self.name())?.build(&(self.1.as_str(),))
    }

    fn name(&self) -> ErrorName<'_> {
        ErrorName::from_static_str_unchecked(self.0.name())
    }

    fn description(&self) -> Option<&str> {
        Some(&self.1)
    }
}

impl From<mxconfig::Error> for Refusal {
    fn from(e: mxconfig::Error) -> Self {
        fail(e.failure(), e.to_string())
    }
}

/// A method's result.
type Reply<T> = Result<T, Refusal>;

/// The D-Bus error a client reads as `failure`.
fn fail(failure: Failure, message: impl Into<String>) -> Refusal {
    Refusal(failure, message.into())
}

/// The host's or one Flatpak app's store, with its source and listeners.
struct Place {
    /// The source used for all reads, writes and watches here.
    source: Source,
    /// The source's roots.
    roots: Roots,
    /// Schemas sent by apps for their own namespaces.
    registered: Mutex<BTreeMap<String, Schema>>,
    /// Each subscribed namespace, held open with its subscribers.
    held: Mutex<BTreeMap<String, Held>>,
}

/// A namespace held open while anyone is subscribed, so its schema stays put.
struct Held {
    settings: Settings,
    /// Each subscription's client, by subscription id.
    subscribers: BTreeMap<u64, OwnedBusName>,
}

struct Inner {
    conn: zbus::Connection,
    /// The host's own place. Its roots name the bus served.
    host: Arc<Place>,
    /// Each Flatpak app's own place, by app id, made on first use.
    apps: Mutex<BTreeMap<String, Arc<Place>>>,
    /// Held while draining outcomes, so signals are sent in order.
    draining: tokio::sync::Mutex<()>,
    /// Each sender's caller info, looked up once.
    clients: Mutex<BTreeMap<String, Caller>>,
    /// The id of the next subscription.
    next: AtomicU64,
    /// Held while looking up a caller, so concurrent calls do it once.
    resolving: tokio::sync::Mutex<()>,
}

/// The mxconfig D-Bus service. Clones share state.
#[derive(Clone)]
pub struct Service {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Service {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Service").finish_non_exhaustive()
    }
}

impl Service {
    /// Serve the namespaces under `roots` that belong on `bus`. Needs Tokio.
    ///
    /// On the session bus that is app and desktop namespaces. On the system bus it is privileged
    /// ones, their admin layer written after polkit approves.
    pub fn new(conn: zbus::Connection, roots: Roots, bus: Bus) -> Self {
        let roots = Roots {
            serving: Some(bus),
            ..roots
        };
        let inner = Arc::new(Inner {
            host: Arc::new(Place::new(roots)),
            conn,
            apps: Mutex::default(),
            draining: tokio::sync::Mutex::default(),
            clients: Mutex::default(),
            next: AtomicU64::new(0),
            resolving: tokio::sync::Mutex::default(),
        });
        watch_files(Arc::clone(&inner), Arc::clone(&inner.host));
        Self { inner }
    }

    /// Serve the interface on its connection, then own the bus name.
    pub async fn publish(&self) -> zbus::Result<()> {
        let conn = &self.inner.conn;
        conn.object_server().at(OBJECT_PATH, self.clone()).await?;
        // Request the name after serving, so no call arrives before the object.
        conn.request_name(BUS_NAME).await
    }

    /// The place of Flatpak app `app`, made over `roots` if it is new.
    fn app_place(&self, app: &str, roots: Roots) -> Arc<Place> {
        let mut apps = self.inner.apps.lock().unwrap();
        if let Some(place) = apps.get(app) {
            return Arc::clone(place);
        }
        let place = Arc::new(Place::new(roots));
        apps.insert(app.to_string(), Arc::clone(&place));
        watch_files(Arc::clone(&self.inner), Arc::clone(&place));
        place
    }

    /// The host's installed namespaces that this daemon serves.
    fn installed(&self) -> Vec<String> {
        let files = self.inner.host.roots.schema_files().into_iter();
        let schemas = files.filter_map(|file| Schema::parse(&fs::read_to_string(file).ok()?).ok());
        let served = schemas.filter(|schema| self.serves(schema.namespace()));
        served
            .map(|schema| schema.namespace().to_string())
            .collect()
    }

    /// The host's place, then each app's.
    fn places(&self) -> Vec<Arc<Place>> {
        let apps = self.inner.apps.lock().unwrap();
        let all = std::iter::once(&self.inner.host).chain(apps.values());
        all.map(Arc::clone).collect()
    }

    /// The number of subscriptions to `namespace`, in any place.
    #[doc(hidden)]
    pub fn subscribers(&self, namespace: &str) -> usize {
        let count = |place: Arc<Place>| {
            let held = place.held.lock().unwrap();
            held.get(namespace).map_or(0, |held| held.subscribers.len())
        };
        self.places().into_iter().map(count).sum()
    }

    /// Look up `sender`. An unknown process counts as a sandbox without an id.
    async fn caller(&self, sender: &str) -> Caller {
        let cached = || self.inner.clients.lock().unwrap().get(sender).cloned();
        if let Some(caller) = cached() {
            return caller;
        }
        // Resolve one at a time, so each sender is looked up once.
        let _resolving = self.inner.resolving.lock().await;
        if let Some(caller) = cached() {
            return caller;
        }
        let caller = if let Ok(dbus) = fdo::DBusProxy::new(&self.inner.conn).await
            && let Ok(name) = BusName::try_from(sender)
            && let Ok(pid) = dbus.get_connection_unix_process_id(name).await
        {
            Sandbox::of(pid)
        } else {
            Some(Sandbox::default())
        };
        let mut clients = self.inner.clients.lock().unwrap();
        // Unique names are never reused, so a departed client only costs memory. Cap it.
        if clients.len() >= MAX_CLIENTS {
            clients.clear();
        }
        clients.insert(sender.to_string(), caller.clone());
        caller
    }

    /// The caller of `hdr`, claiming to be app `claimed`, if it may access `namespace`.
    async fn authorize(
        &self,
        hdr: &Header<'_>,
        claimed: &str,
        namespace: &str,
        access: Access,
    ) -> Reply<Caller> {
        let sender = sender(hdr);
        let caller = self.caller(sender).await;
        let app = policy::app(caller.as_ref(), claimed);
        if !grants(&self.inner.host.roots, app).allow(namespace, access) {
            let why = match app {
                Some(app) => format!("`{app}` may not access `{namespace}`"),
                None => format!("`{sender}` names no app, so it may not access `{namespace}`"),
            };
            return Err(fail(Failure::Denied, why));
        }
        Ok(caller)
    }

    /// The place where `caller` finds `namespace`, if it is served.
    async fn locate(&self, caller: &Caller, namespace: &str) -> Reply<Arc<Place>> {
        if !self.serves(namespace) {
            return Err(unknown(namespace));
        }
        let host = Arc::clone(&self.inner.host);
        if !Class::is_app(namespace) {
            return Ok(host);
        }
        // A Flatpak's own namespace lives in its sandbox.
        if let Some(sandbox) = caller
            && sandbox.app.as_deref() == Some(namespace)
        {
            let roots = sandbox.roots().ok_or_else(|| unknown(namespace))?;
            return Ok(self.app_place(namespace, roots));
        }
        // The host's copy wins, and a native app's registered schema counts.
        if host.knows(namespace) {
            return Ok(host);
        }
        if let Some(place) = self.inner.apps.lock().unwrap().get(namespace) {
            return Ok(Arc::clone(place));
        }
        let app = namespace.to_string();
        let found = tokio::task::spawn_blocking(move || {
            Sandbox::installed(&app).and_then(|app| app.roots())
        });
        let found = found.await;
        Ok(match found.ok().flatten() {
            Some(roots) => self.app_place(namespace, roots),
            None => host,
        })
    }

    /// Check the caller, as app `claimed`, may write `namespace`, then open it where it lives.
    async fn open_to_write(
        &self,
        hdr: &Header<'_>,
        claimed: &str,
        namespace: &str,
    ) -> Reply<(Arc<Place>, Settings)> {
        let caller = self
            .authorize(hdr, claimed, namespace, Access::ReadWrite)
            .await?;
        let place = self.locate(&caller, namespace).await?;
        let settings = place.open(namespace, None)?;
        Ok((place, settings))
    }

    /// The bus served.
    fn bus(&self) -> Option<Bus> {
        self.inner.host.roots.serving
    }

    /// Whether this daemon serves `namespace`'s class.
    fn serves(&self, namespace: &str) -> bool {
        Class::of(namespace).is_ok_and(|class| Some(Bus::serving(class)) == self.bus())
    }

    /// Ask polkit, in privileged mode, once a write is known to be acceptable.
    async fn approve(&self, hdr: &Header<'_>, namespace: &str) -> Reply<()> {
        if self.bus() != Some(Bus::System) {
            return Ok(());
        }
        let sender = sender(hdr);
        let subject = (
            BUS_NAME_SUBJECT,
            HashMap::from([("name", ZValue::from(sender))]),
        );
        let details = HashMap::from([(NAMESPACE_DETAIL, namespace)]);
        let refused = |e: zbus::Error| fail(Failure::Denied, format!("polkit: {e}"));
        let authority = zbus::Proxy::new(&self.inner.conn, POLKIT, POLKIT_PATH, POLKIT_AUTHORITY)
            .await
            .map_err(refused)?;
        let (authorized, _, _): (bool, bool, HashMap<String, String>) = authority
            .call(
                CHECK_AUTHORIZATION,
                &(subject, WRITE_ACTION, details, ALLOW_USER_INTERACTION, ""),
            )
            .await
            .map_err(refused)?;
        if !authorized {
            return Err(fail(
                Failure::Denied,
                format!("`{sender}` is not authorized to write `{namespace}`"),
            ));
        }
        Ok(())
    }
}

impl Place {
    fn new(roots: Roots) -> Self {
        Self {
            source: Source::with_roots(roots.clone()),
            roots,
            registered: Mutex::default(),
            held: Mutex::default(),
        }
    }

    /// Whether `namespace` has a schema here, installed or registered.
    fn knows(&self, namespace: &str) -> bool {
        self.roots.is_installed(namespace)
            || self.registered.lock().unwrap().contains_key(namespace)
    }

    /// The held handle, else `namespace` opened on the best schema known.
    fn open(&self, namespace: &str, own: Option<Schema>) -> Reply<Settings> {
        if let Some(held) = self.held.lock().unwrap().get(namespace) {
            return Ok(held.settings.clone());
        }
        let registered = || self.registered.lock().unwrap().get(namespace).cloned();
        let schema = match own {
            Some(own) => own,
            None if self.roots.is_installed(namespace) => self.roots.schema(namespace)?,
            None => registered().ok_or_else(|| unknown(namespace))?,
        };
        Ok(self.source.open_schema(schema)?)
    }

    /// Hold `settings` open for subscription `id` of client `name`.
    fn hold(&self, namespace: &str, settings: Settings, id: u64, name: OwnedBusName) {
        let mut held = self.held.lock().unwrap();
        let held = held.entry(namespace.to_string()).or_insert_with(|| Held {
            settings,
            subscribers: BTreeMap::new(),
        });
        held.subscribers.insert(id, name);
    }

    /// End subscription `id`. The last one gone closes its namespace.
    fn release(&self, id: u64) {
        let mut held = self.held.lock().unwrap();
        // The next open takes the current schema.
        held.retain(|_, entry| {
            entry.subscribers.remove(&id);
            !entry.subscribers.is_empty()
        });
    }

    /// Record `schema`, an app's own, unless a newer one is already known.
    fn register(&self, namespace: &str, schema: &Schema) {
        let mut registered = self.registered.lock().unwrap();
        let older = |known: &Schema| known.version() < schema.version();
        if registered.get(namespace).is_none_or(older) {
            registered.insert(namespace.to_string(), schema.clone());
        }
    }

    /// `namespace`'s held settings and an emitter for each connection subscribed, if any.
    fn emitters(
        &self,
        conn: &zbus::Connection,
        namespace: &str,
    ) -> Option<(Settings, Vec<(OwnedBusName, SignalEmitter<'static>)>)> {
        let path = ObjectPath::from_static_str_unchecked(OBJECT_PATH);
        let held = self.held.lock().unwrap();
        let held = held.get(namespace)?;
        let emitter = |name: &OwnedBusName| {
            let emitter = SignalEmitter::from_parts(conn.clone(), path.clone());
            (
                name.clone(),
                emitter.set_destination(name.clone().into_inner()),
            )
        };
        // A client with several subscriptions gets one signal.
        let names: BTreeSet<&OwnedBusName> = held.subscribers.values().collect();
        let emitters = names.into_iter().map(emitter).collect();
        Some((held.settings.clone(), emitters))
    }
}

impl Inner {
    /// Send signals for `place`'s outcomes. Returns write `ticket`'s result.
    async fn drain(
        &self,
        place: &Place,
        ticket: Option<Ticket<Write>>,
    ) -> Option<Result<(), mxconfig::Error>> {
        let (mut changed, mut assets) = (BTreeSet::new(), Vec::new());
        let mut written = None;
        for outcome in place.source.outcomes() {
            if let Some(result) = ticket.and_then(|ticket| ticket.written(&outcome)) {
                written = Some(result);
            }
            match outcome {
                Outcome::Changed(change) => {
                    changed.insert(change.namespace);
                }
                Outcome::AssetChanged { namespace, id } => assets.push((namespace, id)),
                Outcome::Diagnosed {
                    namespace,
                    diagnostics,
                } => {
                    for diagnostic in diagnostics {
                        eprintln!("mxconfigd: {namespace}: {diagnostic}");
                    }
                }
                _ => {}
            }
        }
        for namespace in changed {
            self.emit(place, &namespace).await;
        }
        self.emit_assets(place, assets).await;
        written
    }

    /// Start a write in `place`, then send its signals before replying.
    async fn land(
        &self,
        place: &Place,
        start: impl FnOnce() -> Result<Ticket<Write>, mxconfig::Error>,
    ) -> Reply<()> {
        // Hold the lock across the write so the drain sees its `Written`.
        let _turn = self.draining.lock().await;
        let ticket = start()?;
        let written = self.drain(place, Some(ticket)).await;
        let written = written.ok_or_else(|| fail(Failure::Other, "the write did not land"))?;
        Ok(written?)
    }

    /// Send `namespace`'s current state to its subscribers in `place`.
    async fn emit(&self, place: &Place, namespace: &str) {
        let Some((settings, emitters)) = place.emitters(&self.conn, namespace) else {
            return; // nobody subscribed
        };
        let (revision, values) = state(&settings);
        for (destination, emitter) in emitters {
            let delivered = Service::changed(&emitter, namespace, revision, &values).await;
            if let Err(e) = delivered {
                eprintln!("mxconfigd: cannot signal {destination}: {e}");
            }
        }
    }

    /// Send `AssetChanged` with a new fd to each subscriber in `place`.
    async fn emit_assets(&self, place: &Place, assets: Vec<(String, String)>) {
        for (namespace, id) in assets {
            let Some((settings, emitters)) = place.emitters(&self.conn, &namespace) else {
                continue;
            };
            for (destination, emitter) in emitters {
                // Open the current file separately for each client.
                let file: Vec<AssetFile> = open_asset(&settings, &id).into_iter().collect();
                let delivered = Service::asset_changed(&emitter, &namespace, &id, file).await;
                if let Err(e) = delivered {
                    eprintln!("mxconfigd: cannot signal {destination}: {e}");
                }
            }
        }
    }
}

/// The error for a namespace that isn't served.
fn unknown(namespace: &str) -> Refusal {
    fail(Failure::Unknown, format!("unknown namespace `{namespace}`"))
}

/// Read a file a client sent. Only regular files within the size limit.
fn read_upload(fd: OwnedFd) -> Reply<Vec<u8>> {
    let io_error = |e: io::Error| fail(Failure::Other, e.to_string());
    let too_big = || {
        fail(
            Failure::Invalid,
            format!("an asset holds at most {MAX_ASSET} bytes"),
        )
    };
    let mut file = fs::File::from(fd);
    let meta = file.metadata().map_err(io_error)?;
    if !meta.is_file() {
        return Err(fail(Failure::Invalid, "an asset comes as a regular file"));
    }
    if meta.len() > MAX_ASSET {
        return Err(too_big());
    }
    let mut bytes = Vec::new();
    file.rewind()
        .and_then(|()| file.take(MAX_ASSET + 1).read_to_end(&mut bytes))
        .map_err(io_error)?;
    // The file may have grown, so check what was actually read.
    if bytes.len() as u64 > MAX_ASSET {
        return Err(too_big());
    }
    Ok(bytes)
}

/// Feed `place`'s source each read of its inotify fd.
fn watch_files(inner: Arc<Inner>, place: Arc<Place>) {
    let fd = match place.source.inotify_fd() {
        Ok(fd) => fd,
        Err(e) => return eprintln!("mxconfigd: cannot watch files: {e}"),
    };
    tokio::spawn(async move {
        let Some(fd) = register(fd) else {
            return eprintln!("mxconfigd: cannot watch files");
        };
        let mut buf = vec![0u8; INOTIFY_BUFFER];
        // Nothing runs on a timer.
        while let Ok(n) = read(&fd, &mut buf).await {
            place.source.feed_inotify(&buf[..n]);
            let _turn = inner.draining.lock().await;
            inner.drain(&place, None).await;
        }
    });
}

/// `fd` registered with Tokio, to wait until it is readable.
fn register(fd: OwnedFd) -> Option<AsyncFd<fs::File>> {
    // SAFETY: the `File` is owned by the `AsyncFd`, so its fd stays open and unchanged.
    unsafe { AsyncFd::register(fs::File::from(fd)) }.ok()
}

/// Wait until `fd` is readable, then read it into `buf`.
async fn read(fd: &AsyncFd<fs::File>, buf: &mut [u8]) -> io::Result<usize> {
    loop {
        let mut ready = fd.readable().await?;
        if let Ok(read) = ready.try_io(|file| (&mut file.get_ref()).read(buf)) {
            return read;
        }
    }
}

/// A pipe for a subscription: the end the daemon watches, and the end the client keeps.
fn subscription_pipe() -> Reply<(OwnedFd, OwnedFd)> {
    let flags = nix::fcntl::OFlag::O_CLOEXEC | nix::fcntl::OFlag::O_NONBLOCK;
    nix::unistd::pipe2(flags).map_err(|e| fail(Failure::Other, e.to_string()))
}

/// End subscription `id` in `place` once the client's end of `watched` is closed.
fn watch_subscription(place: Arc<Place>, id: u64, watched: OwnedFd) {
    tokio::spawn(async move {
        if let Some(fd) = register(watched) {
            // The client never writes, so the pipe turns readable once its end is closed.
            let mut byte = [0u8; 1];
            while read(&fd, &mut byte).await.is_ok_and(|n| n > 0) {}
        }
        place.release(id);
    });
}

/// The unique name a message came from.
fn sender<'a>(hdr: &'a Header<'_>) -> &'a str {
    hdr.sender().map_or("", |name| name.as_str())
}

/// The revision and values of `settings`, as the wire carries them.
fn state(settings: &Settings) -> (u64, ValueMap) {
    let resolved = settings.resolved();
    let values = resolved.iter();
    let values = values.map(|(key, value)| (key.clone(), ZValue::from(value.to_string())));
    (resolved.revision(), values.collect())
}

/// Open asset `id`'s file read-only. `None` if nothing sets it.
fn open_asset(settings: &Settings, id: &str) -> Option<AssetFile> {
    let asset = settings.asset(id)?;
    match fs::File::open(&asset.path) {
        Ok(file) => Some((
            asset.path.display().to_string(),
            ZFd::from(OwnedFd::from(file)),
        )),
        Err(e) => {
            eprintln!("mxconfigd: cannot open asset {}: {e}", asset.path.display());
            None
        }
    }
}

/// All set assets as (id, path, fd), for a `Subscribe` reply.
fn asset_files(settings: &Settings) -> Vec<(String, String, ZFd)> {
    let schema = settings.describe();
    let opened = schema.assets().iter().filter_map(|decl| {
        let (path, fd) = open_asset(settings, &decl.id)?;
        Some((decl.id.clone(), path, fd))
    });
    opened.collect()
}

// TODO: revisit dbus methods later

/// The D-Bus interface. Its name must match [`protocol::INTERFACE`].
#[zbus::interface(name = "org.mechanix.MxConfig")]
impl Service {
    /// The namespaces this caller, as app `app`, may access.
    async fn list_namespaces(&self, #[zbus(header)] hdr: Header<'_>, app: String) -> Vec<String> {
        let caller = self.caller(sender(&hdr)).await;
        let app = policy::app(caller.as_ref(), &app);
        let grants = grants(&self.inner.host.roots, app);
        let mut names = self.installed();
        for place in self.places() {
            names.extend(place.registered.lock().unwrap().keys().cloned());
        }
        names.sort();
        names.dedup();
        names
            .into_iter()
            .filter(|namespace| grants.allow(namespace, Access::Read))
            .collect()
    }

    /// Apply edits atomically. Sends `Changed`, then returns the revision and values.
    #[zbus(out_args("revision", "values"))]
    async fn apply(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        namespace: String,
        edits: Vec<Entry>,
        app: String,
    ) -> Result<(u64, ValueMap), Refusal> {
        let (place, settings) = self.open_to_write(&hdr, &app, &namespace).await?;
        let edits = edits
            .into_iter()
            .map(entry_edit)
            .collect::<Reply<Vec<_>>>()?;
        settings.check(&edits)?;
        self.approve(&hdr, &namespace).await?;
        self.inner.land(&place, || settings.write(&edits)).await?;
        Ok(state(&settings))
    }

    /// Copy `file` as the user's copy of asset `id`.
    async fn write_asset(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        file: ZFd,
        namespace: String,
        id: String,
        app: String,
    ) -> Result<(), Refusal> {
        let (place, settings) = self.open_to_write(&hdr, &app, &namespace).await?;
        settings.check_asset(&id)?;
        self.approve(&hdr, &namespace).await?;
        let fd = OwnedFd::from(file);
        // Read on a blocking thread, as a portal's file may be slow.
        let read = tokio::task::spawn_blocking(move || read_upload(fd)).await;
        let bytes = read.map_err(|e| fail(Failure::Other, e.to_string()))??;
        self.inner
            .land(&place, || settings.write_asset(&id, &bytes))
            .await
    }

    /// Remove the user's copy of asset `id`.
    async fn clear_asset(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        namespace: String,
        id: String,
        app: String,
    ) -> Result<(), Refusal> {
        let (place, settings) = self.open_to_write(&hdr, &app, &namespace).await?;
        settings.check_asset(&id)?;
        self.approve(&hdr, &namespace).await?;
        self.inner.land(&place, || settings.clear_asset(&id)).await
    }

    /// Follow a namespace while the returned descriptor stays open, registering an app's own schema.
    #[zbus(out_args("subscription", "schema", "revision", "values", "assets"))]
    async fn subscribe(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        namespace: String,
        schema: String,
        app: String,
    ) -> Result<(ZFd, String, u64, ValueMap, Vec<(String, String, ZFd)>), Refusal> {
        let own = (!schema.is_empty())
            .then(|| Schema::parse_for(&namespace, &schema))
            .transpose()
            .map_err(|e| fail(Failure::Invalid, e.to_string()))?;
        let caller = self.authorize(&hdr, &app, &namespace, Access::Read).await?;
        // A client's own app namespace opens on its schema. Others get the served one to judge.
        let trusted = own.filter(|_| {
            self.bus() == Some(Bus::Session)
                && Class::is_app(&namespace)
                && policy::app(caller.as_ref(), &app) == Some(namespace.as_str())
        });
        // A native app with its own schema is a native install.
        let place = match (&trusted, &caller) {
            (Some(_), None) => Arc::clone(&self.inner.host),
            _ => self.locate(&caller, &namespace).await?,
        };
        if let Some(own) = &trusted {
            place.register(&namespace, own);
        }
        let settings = place.open(&namespace, trusted)?;
        // The kernel tells the daemon when the client's end closes, however it ends.
        let (watched, kept) = subscription_pipe()?;
        if let Some(sender) = hdr.sender() {
            let id = self.inner.next.fetch_add(1, Ordering::Relaxed);
            let name = BusName::from(sender.to_owned()).into();
            place.hold(&namespace, settings.clone(), id, name);
            watch_subscription(Arc::clone(&place), id, watched);
        }
        let schema = settings.describe().source().to_string();
        let (revision, values) = state(&settings);
        let assets = asset_files(&settings);
        Ok((ZFd::from(kept), schema, revision, values, assets))
    }

    /// The revision and every set value after a change, as TOML text.
    #[zbus(signal)]
    async fn changed(
        emitter: &SignalEmitter<'_>,
        namespace: &str,
        revision: u64,
        values: &ValueMap,
    ) -> zbus::Result<()>;

    /// An asset's file changed. Empty when nothing sets it now.
    #[zbus(signal)]
    async fn asset_changed(
        emitter: &SignalEmitter<'_>,
        namespace: &str,
        id: &str,
        file: Vec<AssetFile>,
    ) -> zbus::Result<()>;
}
