//! Shared fixtures for the daemon's D-Bus tests.

#![allow(dead_code, reason = "each test binary uses a different part")]

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use futures_util::{Stream, StreamExt};
use mxconfig::host::Roots;
pub use mxconfig::host::protocol::{BUS_NAME, INTERFACE, OBJECT_PATH};
use mxconfig::{Error, FromSettings, Outcome, Settings, Source, Table, Ticket, Write};
use mxconfig_daemon::Service;
use zbus::zvariant::{OwnedValue, Value as ZValue};

mod zbus_link;

pub use zbus_link::ZbusLink;

/// The error a refused write gets.
pub const ACCESS_DENIED: &str = "org.freedesktop.DBus.Error.AccessDenied";

/// The error for a namespace the daemon doesn't serve.
pub const UNKNOWN_OBJECT: &str = "org.freedesktop.DBus.Error.UnknownObject";

/// The error for a key or value the schema rejects.
pub const INVALID_ARGS: &str = "org.freedesktop.DBus.Error.InvalidArgs";

/// `Apply`'s reply: the revision and every set value.
pub type Applied = (u64, BTreeMap<String, OwnedValue>);

/// `Subscribe`'s reply: the subscription, schema, revision, values and assets.
pub type Subscribed = (
    zbus::zvariant::OwnedFd,
    String,
    u64,
    BTreeMap<String, OwnedValue>,
    Vec<(String, String, zbus::zvariant::OwnedFd)>,
);

/// A `Changed` signal's body: namespace, revision and every set value.
pub type Changed = (String, u64, BTreeMap<String, OwnedValue>);

/// The app namespace the daemon serves.
pub const NAMESPACE: &str = "org.example.player";

/// A desktop namespace, which needs a manifest.
pub const DESKTOP: &str = "org.example.desktop.theme";

/// A tool whose manifest grants every namespace the tests install.
pub const TOOL: &str = "org.example.tool";

/// An app schema with two keys and one asset.
pub const SCHEMA: &str = r#"
namespace = "org.example.player"
version = 1

[groups.audio]
label = "Audio"
order = 10

[keys.audio.volume]
type = "integer"
default = 80
label = "Volume"
description = "Output volume"
min = 0
max = 100

[keys.audio.mute]
type = "bool"
default = false
label = "Mute"
description = "Silence the output"

[assets.layout]
path = "layout.yaml"
"#;

/// A shared namespace with one key, for the manifest tests.
pub const DESKTOP_SCHEMA: &str = r##"
namespace = "org.example.desktop.theme"
version = 1

[keys.accent]
type = "string"
default = "#7aa2f7"
label = "Accent"
description = "Accent color"
"##;

/// A privileged namespace, which only the privileged daemon serves.
pub const DISPLAY: &str = "org.example.privileged.display";

/// The privileged schema, with a managed key and an asset.
pub const DISPLAY_SCHEMA: &str = r#"
namespace = "org.example.privileged.display"
version = 1

[keys.brightness]
type = "integer"
default = 50
label = "Brightness"
description = "d"

[keys.mode]
type = "string"
default = "day"
label = "Mode"
description = "d"

[keys.panel]
type = "string"
default = "oled"
label = "Panel"
description = "d"
managed = true

[assets.calibration]
path = "calibration.icc"
"#;

/// A `[[migration]]` that renames `old` to `new` from `from` to `to`.
pub fn rename_edge(from: u32, to: u32, old: &str, new: &str) -> String {
    format!(
        "\n[[migration]]\nfrom = {from}\nto = {to}\nops = [{{ op = \"rename\", from = \"{old}\", to = \"{new}\" }}]\n"
    )
}

/// A private `dbus-daemon`, killed on drop.
pub struct Bus {
    child: std::process::Child,
    pub address: String,
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Start a private session bus. `None` without dbus-daemon.
pub fn start_bus() -> Option<Bus> {
    let mut child = Command::new("dbus-daemon")
        .args(["--session", "--print-address"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| eprintln!("skipping: dbus-daemon is unavailable: {e}"))
        .ok()?;
    let address = BufReader::new(child.stdout.take()?)
        .lines()
        .next()?
        .ok()?
        .trim()
        .to_string();
    Some(Bus { child, address })
}

/// Roots in a temporary directory.
pub fn fixture() -> (Roots, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let roots = Roots::under(tmp.path());
    for dir in [
        &roots.schema_dirs[0],
        roots.vendor_dir.as_ref().unwrap(),
        roots.admin_dir.as_ref().unwrap(),
        roots.user_dir.as_ref().unwrap(),
        roots.manifest_dir.as_ref().unwrap(),
    ] {
        fs::create_dir_all(dir).unwrap();
    }
    install(&roots, NAMESPACE, SCHEMA);
    install(&roots, DESKTOP, DESKTOP_SCHEMA);
    (roots, tmp)
}

/// Install `schema` for `namespace` in the first schema dir, and grant it to [`TOOL`].
pub fn install(roots: &Roots, namespace: &str, schema: &str) {
    put(
        &roots.schema_dirs[0].join(format!("{namespace}.toml")),
        schema,
    );
    grant_tool(roots, namespace);
}

/// Add a read-write grant for `namespace` to [`TOOL`]'s manifest, once.
pub fn grant_tool(roots: &Roots, namespace: &str) {
    let path = roots
        .manifest_dir
        .as_ref()
        .unwrap()
        .join(format!("{TOOL}.toml"));
    let text = read(&path).unwrap_or_else(|| format!("client = \"{TOOL}\"\n"));
    let entry = format!("\n[[settings]]\nnamespace = \"{namespace}\"\naccess = \"read-write\"\n");
    if !text.contains(&entry) {
        put(&path, &(text + &entry));
    }
}

/// Install a manifest for `client`, granting it `access` to `namespace`.
pub fn manifest(roots: &Roots, client: &str, namespace: &str, access: &str) {
    fs::write(
        roots.manifest_dir.as_ref().unwrap().join(format!("{client}.toml")),
        format!(
            "client = \"{client}\"\n\n[[settings]]\nnamespace = \"{namespace}\"\naccess = \"{access}\"\n"
        ),
    )
    .unwrap();
}

/// A connection to `bus`, owning `name` if given.
pub async fn connect(bus: &Bus, name: Option<&str>) -> zbus::Connection {
    let mut builder = zbus::connection::Builder::address(bus.address.as_str()).unwrap();
    if let Some(name) = name {
        builder = builder.name(name).unwrap();
    }
    builder.build().await.unwrap()
}

/// A daemon for `daemon` serving `roots` on `bus`, with its connection.
pub async fn serve(bus: &Bus, roots: &Roots, daemon: mxconfig::Bus) -> (zbus::Connection, Service) {
    let conn = connect(bus, None).await;
    let service = Service::new(conn.clone(), roots.clone(), daemon);
    service.publish().await.unwrap();
    (conn, service)
}

/// The session daemon serving the fixture on a private bus, if dbus-daemon exists.
pub async fn served() -> Option<(Bus, Service, Roots, tempfile::TempDir)> {
    let (roots, tmp) = fixture();
    let bus = start_bus()?;
    let (_, service) = serve(&bus, &roots, mxconfig::Bus::Session).await;
    Some((bus, service, roots, tmp))
}

/// Wait until `namespace` has at most `left` subscriptions.
pub async fn released(service: &Service, namespace: &str, left: usize) {
    let gone = async {
        while service.subscribers(namespace) > left {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), gone)
        .await
        .expect("released");
}

/// The path of `file` in `namespace`'s user directory.
pub fn user_file(roots: &Roots, namespace: &str, file: &str) -> PathBuf {
    roots.user_dir.as_ref().unwrap().join(namespace).join(file)
}

/// The main file of a shared namespace under `base`.
pub fn shared_file(base: Option<&PathBuf>, namespace: &str) -> PathBuf {
    base.unwrap()
        .join("mxconfig")
        .join(namespace)
        .join("conf.toml")
}

/// A proxy over a connection that owns `name`.
pub async fn proxy_named(bus: &Bus, name: &str) -> zbus::Proxy<'static> {
    let conn = connect(bus, Some(name)).await;
    zbus::Proxy::new(&conn, BUS_NAME.to_string(), OBJECT_PATH, INTERFACE)
        .await
        .unwrap()
}

/// A proxy for [`INTERFACE`] at [`OBJECT_PATH`], as the app.
pub async fn proxy(bus: &Bus) -> zbus::Proxy<'static> {
    proxy_named(bus, NAMESPACE).await
}

/// The string in a variant.
pub fn text(value: &ZValue<'_>) -> String {
    match value {
        ZValue::Str(s) => s.to_string(),
        other => format!("{other:?}"),
    }
}

/// The values of a `Subscribe` reply as TOML text, subscribed as [`TOOL`].
pub async fn subscribe(
    proxy: &zbus::Proxy<'_>,
    namespace: &str,
) -> zbus::Result<BTreeMap<String, String>> {
    subscribe_as(proxy, TOOL, namespace).await
}

/// `Subscribe` as `app`. The subscription lasts while the reply is kept.
pub async fn hold_as(
    proxy: &zbus::Proxy<'_>,
    app: &str,
    namespace: &str,
) -> zbus::Result<Subscribed> {
    within(proxy.call("Subscribe", &(namespace, "", app))).await
}

/// The values of a `Subscribe` reply as TOML text, subscribed as `app`.
pub async fn subscribe_as(
    proxy: &zbus::Proxy<'_>,
    app: &str,
    namespace: &str,
) -> zbus::Result<BTreeMap<String, String>> {
    let reply = hold_as(proxy, app, namespace).await?;
    let values = reply
        .3
        .iter()
        .map(|(key, value)| (key.clone(), text(value)));
    Ok(values.collect())
}

/// The next `Changed` signal from `signals`, within ten seconds.
pub async fn next_changed(signals: &mut (impl Stream<Item = zbus::Message> + Unpin)) -> Changed {
    let message = within(signals.next()).await.expect("the stream is open");
    message.body().deserialize().unwrap()
}

/// The version and edges recorded in `namespace`'s user state file.
pub fn recorded(roots: &Roots, namespace: &str) -> (i64, Vec<(i64, i64)>) {
    let path = user_file(roots, namespace, "config.toml.state");
    let state: Table = fs::read_to_string(path).unwrap().parse().unwrap();
    let applied = state.get("applied").and_then(|a| a.as_array());
    let pair = |m: &mxconfig::Value| {
        (
            m["from"].as_integer().unwrap(),
            m["to"].as_integer().unwrap(),
        )
    };
    let edges = applied
        .map(|a| a.iter().map(pair).collect())
        .unwrap_or_default();
    (state["version"].as_integer().unwrap(), edges)
}

/// Wait at most ten seconds for `future`.
pub async fn within<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(10), future)
        .await
        .expect("answered in time")
}

/// A proxy for the daemon on a new unnamed connection.
pub async fn caller(bus: &Bus) -> zbus::Proxy<'static> {
    let conn = connect(bus, None).await;
    zbus::Proxy::new(&conn, BUS_NAME.to_string(), OBJECT_PATH, INTERFACE)
        .await
        .unwrap()
}

/// `Apply` with raw (key, set, TOML text) entries, as [`TOOL`].
pub async fn apply(
    proxy: &zbus::Proxy<'_>,
    namespace: &str,
    edits: &[(&str, bool, &str)],
) -> zbus::Result<Applied> {
    apply_as(proxy, TOOL, namespace, edits).await
}

/// `Apply` with raw entries, as `app`.
pub async fn apply_as(
    proxy: &zbus::Proxy<'_>,
    app: &str,
    namespace: &str,
    edits: &[(&str, bool, &str)],
) -> zbus::Result<Applied> {
    within(proxy.call("Apply", &(namespace, edits.to_vec(), app))).await
}

/// `ListNamespaces` as `app`.
pub async fn list(proxy: &zbus::Proxy<'_>, app: &str) -> zbus::Result<Vec<String>> {
    within(proxy.call("ListNamespaces", &(app,))).await
}

/// `WriteAsset` as [`TOOL`], with a file in `dir` holding `bytes`.
pub async fn write_asset(
    proxy: &zbus::Proxy<'_>,
    dir: &Path,
    namespace: &str,
    id: &str,
    bytes: &str,
) -> zbus::Result<()> {
    let upload = dir.join("upload");
    fs::write(&upload, bytes).unwrap();
    let file = fs::File::open(&upload).unwrap();
    let fd = zbus::zvariant::OwnedFd::from(std::os::fd::OwnedFd::from(file));
    within(proxy.call("WriteAsset", &(fd, namespace, id, TOOL))).await
}

/// `ClearAsset` for asset `id`, as [`TOOL`].
pub async fn clear_asset(proxy: &zbus::Proxy<'_>, namespace: &str, id: &str) -> zbus::Result<()> {
    within(proxy.call("ClearAsset", &(namespace, id, TOOL))).await
}

/// The D-Bus error name of a failed call.
pub fn error_name<T: std::fmt::Debug>(result: zbus::Result<T>) -> String {
    match result {
        Err(zbus::Error::MethodError(name, _, _)) => name.to_string(),
        other => panic!("expected a D-Bus error, got {other:?}"),
    }
}

/// Replace `path` with `text` atomically, as an editor would.
pub fn put(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let tmp = path.with_extension("tmp-next");
    fs::write(&tmp, text).unwrap();
    fs::rename(tmp, path).unwrap();
}

/// The text of `path`, or `None` if it is missing.
pub fn read(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

/// A link carrying `source`'s traffic for `daemon` over `bus`.
pub async fn link(bus: &Bus, daemon: mxconfig::Bus, source: &Source) -> ZbusLink {
    let conn = connect(bus, None).await;
    ZbusLink::new(&conn, daemon, source.clone()).await.unwrap()
}

/// A source with no local files, as [`TOOL`], linked to the session daemon on `bus`.
pub async fn linked(bus: &Bus) -> (Source, ZbusLink) {
    let source = Source::with_roots(Roots::default());
    source.set_app(TOOL);
    let link = link(bus, mxconfig::Bus::Session, &source).await;
    (source, link)
}

/// Open `namespace` through `link`, or the error it got.
pub async fn try_open(
    source: &Source,
    link: &mut ZbusLink,
    namespace: &str,
) -> Result<Settings, Error> {
    let ticket: Ticket<Settings> = source.request_namespace(namespace);
    within(link.settle()).await;
    opened(source, ticket)
}

/// The result `ticket` waits for. Drains `source`'s outcomes.
pub fn opened<T: FromSettings>(source: &Source, ticket: Ticket<T>) -> Result<T, Error> {
    source
        .outcomes()
        .iter()
        .find_map(|o| ticket.opened(o))
        .expect("an Opened outcome")
}

/// Open `namespace` through the daemon on `link`.
pub async fn open(source: &Source, link: &mut ZbusLink, namespace: &str) -> Settings {
    let settings = try_open(source, link, namespace).await.unwrap();
    assert!(settings.from_daemon());
    settings
}

/// The result of write `ticket`, from the outcomes so far.
pub fn written(source: &Source, ticket: Ticket<Write>) -> Result<(), Error> {
    let outcomes = source.outcomes();
    let found = outcomes.iter().find_map(|o| ticket.written(o));
    found.expect("written")
}

/// Run `link` until an outcome matches `wanted`.
pub async fn hear(source: &Source, link: &mut ZbusLink, wanted: impl Fn(&Outcome) -> bool) {
    let heard = async {
        loop {
            assert!(link.next().await, "the bus stayed up");
            if source.outcomes().iter().any(&wanted) {
                return;
            }
        }
    };
    within(heard).await;
}
