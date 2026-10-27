//! Tests the privileged daemon: what it serves, polkit, managed keys and files.

mod common;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{ACCESS_DENIED, Bus, DESKTOP, DISPLAY, DISPLAY_SCHEMA, INVALID_ARGS, NAMESPACE, TOOL};
use common::{UNKNOWN_OBJECT, ZbusLink, connect, next_changed, serve, start_bus, subscribe, text};
use common::{apply, caller, clear_asset, error_name, hear, hold_as, link, list, open, put};
use common::{read, rename_edge, shared_file, within, write_asset, written};
use futures_util::{Stream, StreamExt};
use mxconfig::Bus as Daemon;
use mxconfig::host::Roots;
use mxconfig::host::protocol::WRITE_ACTION;
use mxconfig::runtime::Drive;
use mxconfig::{Edit, ErrorKind, Outcome, Source, Value};
use mxconfig_daemon::Service;
use zbus::zvariant::Value as ZValue;

/// A second privileged namespace.
const POWER: &str = "org.example.privileged.power";

const POWER_SCHEMA: &str = r#"
namespace = "org.example.privileged.power"
version = 1

[keys.profile]
type = "string"
default = "balanced"
label = "Profile"
description = "d"
"#;

/// One `CheckAuthorization` call the fake polkit received.
#[derive(Debug, Clone, PartialEq)]
struct Check {
    subject: String,
    name: String,
    action: String,
    namespace: Option<String>,
    flags: u32,
    cancel: String,
}

/// A fake polkit authority that records each check.
struct Polkit {
    allow: Arc<AtomicBool>,
    checks: Arc<Mutex<Vec<Check>>>,
}

#[zbus::interface(name = "org.freedesktop.PolicyKit1.Authority")]
impl Polkit {
    #[expect(clippy::needless_pass_by_value, reason = "zbus passes owned maps")]
    fn check_authorization(
        &self,
        subject: (&str, HashMap<&str, ZValue<'_>>),
        action: &str,
        details: HashMap<&str, &str>,
        flags: u32,
        cancel: &str,
    ) -> (bool, bool, HashMap<String, String>) {
        self.checks.lock().unwrap().push(Check {
            subject: subject.0.to_string(),
            name: subject.1.get("name").map(text).unwrap_or_default(),
            action: action.to_string(),
            namespace: details.get("namespace").map(ToString::to_string),
            flags,
            cancel: cancel.to_string(),
        });
        (self.allow.load(Ordering::SeqCst), false, HashMap::new())
    }
}

/// The fake polkit's connection, its answer and what it was asked.
struct Authority {
    allow: Arc<AtomicBool>,
    checks: Arc<Mutex<Vec<Check>>>,
    _conn: zbus::Connection,
}

impl Authority {
    fn checks(&self) -> Vec<Check> {
        self.checks.lock().unwrap().clone()
    }
}

/// Serve a fake polkit on `bus` that allows everything at first.
async fn polkit(bus: &Bus) -> Authority {
    let allow = Arc::new(AtomicBool::new(true));
    let checks = Arc::new(Mutex::new(Vec::new()));
    let conn = zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .name("org.freedesktop.PolicyKit1")
        .unwrap()
        .serve_at(
            "/org/freedesktop/PolicyKit1/Authority",
            Polkit {
                allow: Arc::clone(&allow),
                checks: Arc::clone(&checks),
            },
        )
        .unwrap()
        .build()
        .await
        .unwrap();
    Authority {
        allow,
        checks,
        _conn: conn,
    }
}

/// Roots with privileged, app and desktop schemas installed.
fn installed() -> (Roots, tempfile::TempDir) {
    let (roots, tmp) = common::fixture();
    std::fs::create_dir_all(roots.admin_dir.as_ref().unwrap().join("mxconfig")).unwrap();
    common::install(&roots, DISPLAY, DISPLAY_SCHEMA);
    common::install(&roots, POWER, POWER_SCHEMA);
    (roots, tmp)
}

/// A private bus with the privileged daemon and, if asked, a fake polkit.
struct Rig {
    bus: Bus,
    polkit: Option<Authority>,
    server: (zbus::Connection, Service),
    roots: Roots,
    tmp: tempfile::TempDir,
}

impl Rig {
    fn polkit(&self) -> &Authority {
        self.polkit.as_ref().expect("a fake polkit")
    }
}

/// A rig over [`installed`] roots. `None` without dbus-daemon.
async fn rig(with_polkit: bool) -> Option<Rig> {
    let (roots, tmp) = installed();
    rig_over(roots, tmp, with_polkit).await
}

/// A rig over `roots`. `None` without dbus-daemon.
async fn rig_over(roots: Roots, tmp: tempfile::TempDir, with_polkit: bool) -> Option<Rig> {
    let bus = start_bus()?;
    let polkit = if with_polkit {
        Some(polkit(&bus).await)
    } else {
        None
    };
    let server = serve(&bus, &roots, Daemon::System).await;
    Some(Rig {
        bus,
        polkit,
        server,
        roots,
        tmp,
    })
}

/// The unique name of `proxy`'s connection.
fn unique_name(proxy: &zbus::Proxy<'_>) -> String {
    proxy.connection().unique_name().unwrap().to_string()
}

/// Wait for a `Changed` that sets `key` to the TOML `value`.
async fn changed_to(
    signals: &mut (impl Stream<Item = zbus::Message> + Unpin),
    key: &str,
    value: &str,
) -> u64 {
    loop {
        let (namespace, revision, values) = next_changed(signals).await;
        assert_eq!(namespace, DISPLAY);
        if values.get(key).is_some_and(|v| text(v) == value) {
            return revision;
        }
    }
}

/// `ListNamespaces` on the privileged daemon lists privileged namespaces only.
#[tokio::test(flavor = "multi_thread")]
async fn lists_only_privileged_namespaces() {
    let Some(rig) = rig(true).await else {
        return;
    };
    let proxy = caller(&rig.bus).await;
    let listed = list(&proxy, TOOL).await.unwrap();
    assert_eq!(listed, vec![DISPLAY.to_string(), POWER.to_string()]);
}

/// App and desktop namespaces are neither read nor written by it.
#[tokio::test(flavor = "multi_thread")]
async fn refuses_app_and_desktop_namespaces() {
    let Some(rig) = rig(true).await else {
        return;
    };
    let proxy = caller(&rig.bus).await;
    for namespace in [NAMESPACE, DESKTOP] {
        let name = error_name(subscribe(&proxy, namespace).await);
        assert_eq!(name, UNKNOWN_OBJECT, "{namespace}");
    }
    let name = error_name(apply(&proxy, NAMESPACE, &[("audio.volume", true, "5")]).await);
    assert_eq!(name, UNKNOWN_OBJECT);
    let name = error_name(apply(&proxy, DESKTOP, &[("accent", true, "\"#000\"")]).await);
    assert_eq!(name, UNKNOWN_OBJECT);
    // Granted but not installed.
    let nothing = "org.example.privileged.nothing";
    common::grant_tool(&rig.roots, nothing);
    let name = error_name(subscribe(&proxy, nothing).await);
    assert_eq!(name, UNKNOWN_OBJECT);

    // Nothing was written for them anywhere.
    let admin = rig.roots.admin_dir.as_ref().unwrap();
    assert!(!admin.join(NAMESPACE).exists());
    assert!(!admin.join("mxconfig").join(DESKTOP).exists());
    let user = rig.roots.user_dir.as_ref().unwrap();
    assert!(!user.join(NAMESPACE).join("config.toml").exists());
    assert_eq!(rig.polkit().checks(), Vec::new(), "refused before polkit");
}

/// Each write asks polkit once, with the action, namespace and interactive flag.
#[tokio::test(flavor = "multi_thread")]
async fn a_write_asks_polkit_once_with_the_right_details() {
    let Some(rig) = rig(true).await else {
        return;
    };
    let proxy = caller(&rig.bus).await;

    // Reads never ask.
    let _held = hold_as(&proxy, TOOL, DISPLAY).await.unwrap();
    list(&proxy, TOOL).await.unwrap();
    assert_eq!(rig.polkit().checks(), Vec::new());

    apply(&proxy, DISPLAY, &[("brightness", true, "60")])
        .await
        .unwrap();
    apply(
        &proxy,
        DISPLAY,
        &[("brightness", true, "61"), ("mode", true, "\"night\"")],
    )
    .await
    .unwrap();
    let expected = Check {
        subject: "system-bus-name".into(),
        name: unique_name(&proxy),
        action: WRITE_ACTION.into(),
        namespace: Some(DISPLAY.into()),
        flags: 1,
        cancel: String::new(),
    };
    assert_eq!(rig.polkit().checks(), vec![expected.clone(), expected]);
}

/// An approved write lands in the admin file, never the user's.
#[tokio::test(flavor = "multi_thread")]
async fn an_approved_write_lands_in_the_admin_file() {
    let Some(rig) = rig(true).await else {
        return;
    };
    let proxy = caller(&rig.bus).await;
    let (revision, _) = apply(&proxy, DISPLAY, &[("brightness", true, "70")])
        .await
        .unwrap();
    assert!(revision >= 1);
    let file = shared_file(rig.roots.admin_dir.as_ref(), DISPLAY);
    let admin = read(&file).expect("the admin file");
    assert!(admin.contains("brightness = 70"), "{admin}");
    let user = rig.roots.user_dir.as_ref().unwrap();
    assert!(
        !user.join("mxconfig").join(DISPLAY).exists(),
        "no user layer"
    );

    let values = subscribe(&proxy, DISPLAY).await.unwrap();
    assert_eq!(values.get("brightness").map(String::as_str), Some("70"));
    assert_eq!(values.get("panel").map(String::as_str), Some("\"oled\""));
}

/// Polkit refusing is `AccessDenied` and changes nothing on disk.
#[tokio::test(flavor = "multi_thread")]
async fn a_polkit_refusal_is_access_denied_and_writes_nothing() {
    let Some(rig) = rig(true).await else {
        return;
    };
    let file = shared_file(rig.roots.admin_dir.as_ref(), DISPLAY);
    put(&file, "brightness = 10\n");
    rig.polkit().allow.store(false, Ordering::SeqCst);
    let proxy = caller(&rig.bus).await;

    let refused = apply(&proxy, DISPLAY, &[("brightness", true, "90")]).await;
    assert_eq!(error_name(refused), ACCESS_DENIED);
    assert_eq!(read(&file).as_deref(), Some("brightness = 10\n"));
    assert_eq!(rig.polkit().checks().len(), 1);

    let values = subscribe(&proxy, DISPLAY).await.unwrap();
    assert_eq!(values.get("brightness").map(String::as_str), Some("10"));
}

/// Without polkit on the bus a write is `AccessDenied`, and promptly.
#[tokio::test(flavor = "multi_thread")]
async fn without_polkit_a_write_is_access_denied_without_hanging() {
    let Some(rig) = rig(false).await else {
        return;
    };
    let proxy = caller(&rig.bus).await;
    let refused = apply(&proxy, DISPLAY, &[("brightness", true, "90")]).await;
    assert_eq!(error_name(refused), ACCESS_DENIED);
    assert!(!shared_file(rig.roots.admin_dir.as_ref(), DISPLAY).exists());

    // A client sees a denial, not a fallback or a hang.
    let (source, mut link) = system_client(&rig.bus, &rig.roots).await;
    let settings = open(&source, &mut link, DISPLAY).await;
    let ticket = settings.write(&[Edit::set("brightness", 90)]).unwrap();
    within(link.settle()).await;
    let written = written(&source, ticket);
    assert!(
        matches!(&written, Err(e) if e.kind() == ErrorKind::Denied),
        "{written:?}"
    );
    assert_eq!(settings.get("brightness"), Some(Value::Integer(50)));
}

/// A raw write touching a managed key is refused and the file is untouched.
#[tokio::test(flavor = "multi_thread")]
async fn a_raw_managed_write_is_refused_and_the_admin_file_is_untouched() {
    let Some(rig) = rig(true).await else {
        return;
    };
    let file = shared_file(rig.roots.admin_dir.as_ref(), DISPLAY);
    let before = "brightness = 10\npanel = \"lcd\"\n";
    put(&file, before);
    let proxy = caller(&rig.bus).await;

    let batches: [&[(&str, bool, &str)]; 4] = [
        &[("panel", true, "\"amoled\"")],
        &[("panel", false, "")],
        &[("brightness", true, "20"), ("panel", true, "\"amoled\"")],
        &[("panel", true, "\"amoled\""), ("mode", true, "\"night\"")],
    ];
    for edits in batches {
        let refused = apply(&proxy, DISPLAY, edits).await;
        assert_eq!(error_name(refused), ACCESS_DENIED, "{edits:?}");
        assert_eq!(read(&file).as_deref(), Some(before), "{edits:?}");
    }
    let values = subscribe(&proxy, DISPLAY).await.unwrap();
    assert_eq!(values.get("brightness").map(String::as_str), Some("10"));
    assert_eq!(values.get("panel").map(String::as_str), Some("\"lcd\""));
}

/// A managed write is refused before polkit can prompt for it.
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_write_never_asks_polkit() {
    let Some(rig) = rig(true).await else {
        return;
    };
    let proxy = caller(&rig.bus).await;
    let refused = apply(&proxy, DISPLAY, &[("panel", true, "\"amoled\"")]).await;
    assert_eq!(error_name(refused), ACCESS_DENIED);
    // A value the schema rejects, or an unknown key, is refused first too.
    let refused = apply(&proxy, DISPLAY, &[("brightness", true, "\"bright\"")]).await;
    assert_eq!(error_name(refused), INVALID_ARGS);
    let refused = apply(&proxy, DISPLAY, &[("nope", true, "1")]).await;
    assert_eq!(error_name(refused), INVALID_ARGS);
    assert_eq!(
        rig.polkit().checks(),
        Vec::new(),
        "no prompt for any of them"
    );
}

/// A source over `roots` linked to the privileged daemon only.
async fn system_client(bus: &Bus, roots: &Roots) -> (Source, ZbusLink) {
    let source = Source::with_roots(roots.clone());
    source.set_app(TOOL);
    let link = link(bus, Daemon::System, &source).await;
    (source, link)
}

/// A client on the system bus refuses managed keys before sending anything.
#[tokio::test(flavor = "multi_thread")]
async fn a_system_client_refuses_managed_keys_locally() {
    let Some(rig) = rig(true).await else {
        return;
    };
    let file = shared_file(rig.roots.admin_dir.as_ref(), DISPLAY);
    put(&file, "panel = \"lcd\"\n");
    let (source, mut link) = system_client(&rig.bus, &rig.roots).await;
    let settings = open(&source, &mut link, DISPLAY).await;
    assert_eq!(settings.get("panel"), Some(Value::String("lcd".into())));

    let batches = [
        vec![Edit::set("panel", "amoled")],
        vec![Edit::remove("panel")],
        vec![Edit::set("brightness", 20), Edit::set("panel", "amoled")],
    ];
    for edits in &batches {
        let refused = settings.write(edits).unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::Denied, "{refused}");
    }
    assert!(source.take_calls(Daemon::System).is_empty(), "nothing sent");
    within(link.settle()).await;
    assert_eq!(rig.polkit().checks(), Vec::new());
    assert_eq!(read(&file).as_deref(), Some("panel = \"lcd\"\n"));

    // An unmanaged write from the same client still lands.
    let ticket = settings.write(&[Edit::set("brightness", 20)]).unwrap();
    within(link.settle()).await;
    written(&source, ticket).unwrap();
    assert_eq!(settings.get("brightness"), Some(Value::Integer(20)));
    assert_eq!(settings.get("panel"), Some(Value::String("lcd".into())));
    let admin = read(&file).unwrap();
    assert!(admin.contains("brightness = 20") && admin.contains("panel = \"lcd\""));
}

/// `WriteAsset` and `ClearAsset` on a privileged namespace are refused.
#[tokio::test(flavor = "multi_thread")]
async fn asset_writes_to_a_privileged_namespace_are_refused() {
    let Some(rig) = rig(true).await else {
        return;
    };
    let vendor = rig.roots.vendor_dir.as_ref().unwrap();
    let vendor = vendor
        .join("mxconfig")
        .join(DISPLAY)
        .join("calibration.icc");
    put(&vendor, "vendor");
    let proxy = caller(&rig.bus).await;

    let sent = write_asset(&proxy, rig.tmp.path(), DISPLAY, "calibration", "mine").await;
    assert_eq!(error_name(sent), ACCESS_DENIED);
    let sent = clear_asset(&proxy, DISPLAY, "calibration").await;
    assert_eq!(error_name(sent), ACCESS_DENIED);

    let admin = shared_file(rig.roots.admin_dir.as_ref(), DISPLAY);
    assert!(!admin.with_file_name("calibration.icc").exists());
    assert_eq!(read(&vendor).as_deref(), Some("vendor"));

    // The library refuses them too, before sending.
    let (source, mut link) = system_client(&rig.bus, &rig.roots).await;
    let settings = open(&source, &mut link, DISPLAY).await;
    assert_eq!(
        settings
            .write_asset("calibration", b"x")
            .unwrap_err()
            .kind(),
        ErrorKind::Denied
    );
    assert_eq!(
        settings.clear_asset("calibration").unwrap_err().kind(),
        ErrorKind::Denied
    );
    assert!(source.take_calls(Daemon::System).is_empty());
}

/// A hand edit of the admin file is signalled, and later writes merge with it.
#[tokio::test(flavor = "multi_thread")]
async fn a_hand_edit_of_the_admin_file_is_seen_and_merged() {
    let Some(rig) = rig(true).await else {
        return;
    };
    let proxy = caller(&rig.bus).await;
    apply(&proxy, DISPLAY, &[("brightness", true, "70")])
        .await
        .unwrap();
    let _held = hold_as(&proxy, TOOL, DISPLAY).await.unwrap();
    let mut signals = Box::pin(proxy.receive_signal("Changed").await.unwrap());

    let file = shared_file(rig.roots.admin_dir.as_ref(), DISPLAY);
    put(&file, "brightness = 33\npanel = \"lcd\"\n");
    // The subscription opened the namespace afresh, so it counts from there.
    let revision = changed_to(&mut signals, "brightness", "33").await;
    assert!(revision >= 1);

    apply(&proxy, DISPLAY, &[("mode", true, "\"night\"")])
        .await
        .unwrap();
    let admin = read(&file).unwrap();
    for line in ["brightness = 33", "panel = \"lcd\"", "mode = \"night\""] {
        assert!(admin.contains(line), "{line} in {admin}");
    }
    let values = subscribe(&proxy, DISPLAY).await.unwrap();
    assert_eq!(values.get("brightness").map(String::as_str), Some("33"));
    assert_eq!(values.get("panel").map(String::as_str), Some("\"lcd\""));
}

/// A hand edit is seen after the daemon itself made the admin directory.
#[tokio::test(flavor = "multi_thread")]
async fn a_hand_edit_is_seen_after_the_daemon_made_the_admin_directory() {
    let (roots, tmp) = installed();
    std::fs::remove_dir(roots.admin_dir.as_ref().unwrap().join("mxconfig")).unwrap();
    let Some(rig) = rig_over(roots, tmp, true).await else {
        return;
    };
    let proxy = caller(&rig.bus).await;
    let _held = hold_as(&proxy, TOOL, DISPLAY).await.unwrap();
    let mut signals = Box::pin(proxy.receive_signal("Changed").await.unwrap());
    apply(&proxy, DISPLAY, &[("brightness", true, "70")])
        .await
        .unwrap();
    changed_to(&mut signals, "brightness", "70").await;

    put(
        &shared_file(rig.roots.admin_dir.as_ref(), DISPLAY),
        "brightness = 33\n",
    );
    changed_to(&mut signals, "brightness", "33").await;
}

/// A newer privileged schema waits for the subscriber to go, then migrates the admin file.
#[tokio::test(flavor = "multi_thread")]
async fn a_newer_privileged_schema_migrates_the_admin_file_once_released() {
    let Some(rig) = rig(true).await else {
        return;
    };
    let proxy = caller(&rig.bus).await;
    apply(&proxy, DISPLAY, &[("brightness", true, "70")])
        .await
        .unwrap();
    let held = hold_as(&proxy, TOOL, DISPLAY).await.unwrap();
    let mut signals = Box::pin(proxy.receive_signal("Changed").await.unwrap());

    // An update installs v2, which renames the key.
    let v2 = DISPLAY_SCHEMA
        .replace("version = 1", "version = 2")
        .replace("[keys.brightness]", "[keys.level]")
        + &rename_edge(1, 2, "brightness", "level");
    put(
        &rig.roots.schema_dirs[0].join(format!("{DISPLAY}.toml")),
        &v2,
    );
    let quiet = tokio::time::timeout(Duration::from_millis(300), signals.next()).await;
    assert!(quiet.is_err(), "a held namespace keeps its schema");
    let values = subscribe(&proxy, DISPLAY).await.unwrap();
    assert_eq!(values.get("brightness").map(String::as_str), Some("70"));
    assert!(
        read(&shared_file(rig.roots.admin_dir.as_ref(), DISPLAY))
            .unwrap()
            .contains("brightness = 70")
    );

    // The last subscriber leaving lets the next open take v2.
    drop(held);
    common::released(&rig.server.1, DISPLAY, 0).await;
    let reply = hold_as(&proxy, TOOL, DISPLAY).await.unwrap();
    assert!(reply.1.contains("version = 2"), "the new schema is served");
    let values = subscribe(&proxy, DISPLAY).await.unwrap();
    assert_eq!(values.get("level").map(String::as_str), Some("70"));
    assert_eq!(values.get("brightness"), None);
    let admin = read(&shared_file(rig.roots.admin_dir.as_ref(), DISPLAY)).unwrap();
    assert!(admin.contains("level = 70"), "{admin}");
    assert!(!admin.contains("brightness"), "{admin}");

    // The next write uses the new key and keeps the migrated value's file.
    apply(&proxy, DISPLAY, &[("mode", true, "\"night\"")])
        .await
        .unwrap();
    let admin = read(&shared_file(rig.roots.admin_dir.as_ref(), DISPLAY)).unwrap();
    assert!(admin.contains("level = 70") && admin.contains("mode = \"night\""));
    let old = apply(&proxy, DISPLAY, &[("brightness", true, "1")]).await;
    assert_eq!(error_name(old), INVALID_ARGS, "the old key is gone");
}

/// Both daemons, each on its own private bus, as on a device.
struct Device {
    session: Bus,
    system: Bus,
    polkit: Authority,
    _daemons: [(zbus::Connection, Service); 2],
    roots: Roots,
    _tmp: tempfile::TempDir,
}

/// Start a session and a privileged daemon over the same roots.
async fn device() -> Option<Device> {
    let (roots, tmp) = installed();
    let session = start_bus()?;
    let system = start_bus()?;
    let polkit = polkit(&system).await;
    let daemons = [
        serve(&session, &roots, Daemon::Session).await,
        serve(&system, &roots, Daemon::System).await,
    ];
    Some(Device {
        session,
        system,
        polkit,
        _daemons: daemons,
        roots,
        _tmp: tmp,
    })
}

/// A source over `roots` with a link to each bus.
async fn two_bus_client(device: &Device, roots: Roots) -> (Source, ZbusLink, ZbusLink) {
    let source = Source::with_roots(roots);
    source.set_app(TOOL);
    let mut session = link(&device.session, Daemon::Session, &source).await;
    // Answers the owner query sent on reaching the session bus.
    within(session.settle()).await;
    let system = link(&device.system, Daemon::System, &source).await;
    (source, session, system)
}

/// The privileged daemon takes a write and tells its other subscribers.
#[tokio::test(flavor = "multi_thread")]
async fn a_two_bus_client_writes_a_privileged_namespace_through_the_system_bus() {
    let Some(device) = device().await else {
        return;
    };
    let (source, mut session, mut system) = two_bus_client(&device, Roots::default()).await;
    let settings = open(&source, &mut system, DISPLAY).await;
    let (other, _, mut other_system) = two_bus_client(&device, Roots::default()).await;
    let theirs = open(&other, &mut other_system, DISPLAY).await;

    let ticket = settings.write(&[Edit::set("brightness", 70)]).unwrap();
    assert!(
        source.take_calls(Daemon::Session).is_empty(),
        "not to the session"
    );
    within(system.settle()).await;
    let outcomes = source.outcomes();
    let result = outcomes.iter().find_map(|o| ticket.written(o));
    result.expect("written").unwrap();
    let changed = |o: &Outcome| matches!(o, Outcome::Changed(c) if c.key == "brightness");
    assert!(outcomes.iter().any(changed), "{outcomes:?}");
    assert_eq!(settings.get("brightness"), Some(Value::Integer(70)));
    let admin = read(&shared_file(device.roots.admin_dir.as_ref(), DISPLAY)).unwrap();
    assert!(admin.contains("brightness = 70"), "{admin}");
    assert_eq!(device.polkit.checks().len(), 1);

    // The other subscriber hears it from the privileged daemon.
    let seventy =
        |o: &Outcome| matches!(o, Outcome::Changed(c) if c.value == Some(Value::Integer(70)));
    hear(&other, &mut other_system, seventy).await;
    assert_eq!(theirs.get("brightness"), Some(Value::Integer(70)));
    within(session.settle()).await;
}

/// Publish `service` once the old owner has left.
async fn republish(service: &Service) {
    let taken = async {
        while service.publish().await.is_err() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    within(taken).await;
}

/// The privileged daemon restarting does not break a client's next write.
#[tokio::test(flavor = "multi_thread")]
async fn a_restarted_privileged_daemon_takes_the_next_write() {
    let Some(bus) = start_bus() else {
        return;
    };
    let (roots, _tmp) = installed();
    let authority = polkit(&bus).await;
    let (old, _service) = serve(&bus, &roots, Daemon::System).await;
    let (source, mut link) = system_client(&bus, &roots).await;
    let settings = open(&source, &mut link, DISPLAY).await;

    let ticket = settings.write(&[Edit::set("brightness", 60)]).unwrap();
    within(link.settle()).await;
    written(&source, ticket).unwrap();

    // The old process dies, and a new one takes the name.
    within(old.close()).await.unwrap();
    let conn = connect(&bus, None).await;
    let service = Service::new(conn, roots.clone(), Daemon::System);
    republish(&service).await;

    let ticket = settings
        .write(&[Edit::set("mode", "night"), Edit::set("brightness", 61)])
        .unwrap();
    within(link.settle()).await;
    written(&source, ticket).unwrap();
    assert_eq!(settings.get("brightness"), Some(Value::Integer(61)));
    assert_eq!(settings.get("mode"), Some(Value::String("night".into())));
    let admin = read(&shared_file(roots.admin_dir.as_ref(), DISPLAY)).unwrap();
    assert!(admin.contains("brightness = 61") && admin.contains("mode = \"night\""));
    assert_eq!(authority.checks().len(), 2);
}
