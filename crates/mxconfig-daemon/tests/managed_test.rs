//! Tests the session daemon with managed keys and privileged namespaces.

mod common;

use std::path::PathBuf;

use common::{ACCESS_DENIED, Bus, DISPLAY, DISPLAY_SCHEMA, TOOL, UNKNOWN_OBJECT};
use common::{apply, caller, clear_asset, error_name, fixture, hold_as, linked, list, open};
use common::{put, read, serve, shared_file, start_bus, subscribe, try_open, write_asset};
use mxconfig::Bus as Daemon;
use mxconfig::host::Roots;
use mxconfig::{ErrorKind, Origin, Value};
use mxconfig_daemon::Service;

/// An app namespace with managed and unmanaged keys and assets.
const KIOSK: &str = "org.example.kiosk";

const KIOSK_SCHEMA: &str = r#"
namespace = "org.example.kiosk"
version = 1

[keys.volume]
type = "integer"
default = 50
label = "Volume"
description = "d"

[keys.motd]
type = "string"
default = "hello"
label = "Message"
description = "d"

[keys.locked]
type = "bool"
default = false
label = "Locked"
description = "d"
managed = true

[assets.banner]
path = "banner.txt"
managed = true

[assets.wallpaper]
path = "wallpaper.txt"
"#;

/// The session daemon over the fixture plus the kiosk and display schemas.
struct Rig {
    bus: Bus,
    _server: (zbus::Connection, Service),
    roots: Roots,
    tmp: tempfile::TempDir,
}

/// Start the session daemon. `None` without dbus-daemon.
async fn rig() -> Option<Rig> {
    let (roots, tmp) = fixture();
    common::install(&roots, KIOSK, KIOSK_SCHEMA);
    common::install(&roots, DISPLAY, DISPLAY_SCHEMA);
    let bus = start_bus()?;
    let server = serve(&bus, &roots, Daemon::Session).await;
    Some(Rig {
        bus,
        _server: server,
        roots,
        tmp,
    })
}

/// `file` in an app namespace's home under `base`.
fn app_file(base: Option<&PathBuf>, namespace: &str, file: &str) -> PathBuf {
    base.unwrap().join(namespace).join(file)
}

/// Admin and user files for the kiosk. The user's `locked` and banner are ignored.
fn kiosk_files(roots: &Roots) {
    let (admin, user) = (roots.admin_dir.as_ref(), roots.user_dir.as_ref());
    put(
        &app_file(admin, KIOSK, "config.toml"),
        "locked = true\nvolume = 20\n",
    );
    put(
        &app_file(user, KIOSK, "config.toml"),
        "locked = false\nvolume = 10\nmotd = \"mine\"\n",
    );
    let vendor = roots.vendor_dir.as_ref();
    put(&app_file(vendor, KIOSK, "banner.txt"), "vendor banner");
    put(&app_file(user, KIOSK, "banner.txt"), "user banner");
    put(
        &app_file(vendor, KIOSK, "wallpaper.txt"),
        "vendor wallpaper",
    );
    put(&app_file(user, KIOSK, "wallpaper.txt"), "user wallpaper");
}

/// The session daemon lists managed namespaces but no privileged one.
#[tokio::test(flavor = "multi_thread")]
async fn the_session_daemon_lists_managed_namespaces_but_no_privileged_one() {
    let Some(rig) = rig().await else {
        return;
    };
    let proxy = caller(&rig.bus).await;
    let listed = list(&proxy, TOOL).await.unwrap();
    for namespace in [KIOSK, common::NAMESPACE, common::DESKTOP] {
        assert!(listed.iter().any(|n| n == namespace), "{namespace}");
    }
    assert!(!listed.iter().any(|n| n == DISPLAY), "{listed:?}");
}

/// A daemon-backed client sees admin values for managed keys and assets.
#[tokio::test(flavor = "multi_thread")]
async fn a_client_reads_managed_keys_from_the_admin_layer_through_the_daemon() {
    let Some(rig) = rig().await else {
        return;
    };
    kiosk_files(&rig.roots);
    let (source, mut link) = linked(&rig.bus).await;
    let settings = open(&source, &mut link, KIOSK).await;

    assert_eq!(settings.get("locked"), Some(Value::Boolean(true)), "admin");
    assert_eq!(settings.get("volume"), Some(Value::Integer(10)), "user");
    assert_eq!(settings.get("motd"), Some(Value::String("mine".into())));
    assert_eq!(settings.resolved().origin("locked"), Some(&Origin::Daemon));

    let banner = settings.asset("banner").expect("served");
    assert_eq!(
        banner.text().unwrap(),
        "vendor banner",
        "the user copy is ignored"
    );
    let wallpaper = settings.asset("wallpaper").expect("served");
    assert_eq!(wallpaper.text().unwrap(), "user wallpaper");

    // The raw snapshot agrees.
    let values = subscribe(&caller(&rig.bus).await, KIOSK).await.unwrap();
    assert_eq!(values.get("locked").map(String::as_str), Some("true"));
    assert_eq!(values.get("volume").map(String::as_str), Some("10"));
}

/// A raw `Apply` touching a managed key is `AccessDenied` and writes nothing.
#[tokio::test(flavor = "multi_thread")]
async fn apply_refuses_a_managed_key_and_leaves_the_user_file_untouched() {
    let Some(rig) = rig().await else {
        return;
    };
    kiosk_files(&rig.roots);
    let user = app_file(rig.roots.user_dir.as_ref(), KIOSK, "config.toml");
    let admin = app_file(rig.roots.admin_dir.as_ref(), KIOSK, "config.toml");
    let (user_before, admin_before) = (read(&user), read(&admin));
    let proxy = caller(&rig.bus).await;

    let batches: [&[(&str, bool, &str)]; 4] = [
        &[("locked", true, "false")],
        &[("locked", false, "")],
        &[("volume", true, "30"), ("locked", true, "false")],
        &[("locked", true, "false"), ("motd", true, "\"x\"")],
    ];
    for edits in batches {
        let refused = apply(&proxy, KIOSK, edits).await;
        assert_eq!(error_name(refused), ACCESS_DENIED, "{edits:?}");
        assert_eq!(read(&user), user_before, "{edits:?}");
        assert_eq!(read(&admin), admin_before, "{edits:?}");
    }
    let values = subscribe(&proxy, KIOSK).await.unwrap();
    assert_eq!(values.get("volume").map(String::as_str), Some("10"));
    assert_eq!(values.get("locked").map(String::as_str), Some("true"));
}

/// A raw `Apply` with only unmanaged keys succeeds beside managed ones.
#[tokio::test(flavor = "multi_thread")]
async fn apply_with_only_unmanaged_keys_succeeds() {
    let Some(rig) = rig().await else {
        return;
    };
    kiosk_files(&rig.roots);
    let proxy = caller(&rig.bus).await;
    apply(
        &proxy,
        KIOSK,
        &[("volume", true, "30"), ("motd", false, "")],
    )
    .await
    .unwrap();

    let user = read(&app_file(rig.roots.user_dir.as_ref(), KIOSK, "config.toml")).unwrap();
    assert!(user.contains("volume = 30"), "{user}");
    assert!(!user.contains("motd"), "{user}");
    let values = subscribe(&proxy, KIOSK).await.unwrap();
    assert_eq!(values.get("volume").map(String::as_str), Some("30"));
    assert_eq!(values.get("motd").map(String::as_str), Some("\"hello\""));
    assert_eq!(values.get("locked").map(String::as_str), Some("true"));
}

/// `WriteAsset` and `ClearAsset` on a managed asset are `AccessDenied`.
#[tokio::test(flavor = "multi_thread")]
async fn asset_writes_to_a_managed_asset_are_refused() {
    let Some(rig) = rig().await else {
        return;
    };
    kiosk_files(&rig.roots);
    let proxy = caller(&rig.bus).await;
    let banner = app_file(rig.roots.user_dir.as_ref(), KIOSK, "banner.txt");

    let refused = write_asset(&proxy, rig.tmp.path(), KIOSK, "banner", "new").await;
    assert_eq!(error_name(refused), ACCESS_DENIED);
    let refused = clear_asset(&proxy, KIOSK, "banner").await;
    assert_eq!(error_name(refused), ACCESS_DENIED);
    assert_eq!(read(&banner).as_deref(), Some("user banner"), "left alone");

    // An unmanaged asset beside it is written as usual.
    write_asset(&proxy, rig.tmp.path(), KIOSK, "wallpaper", "new")
        .await
        .unwrap();
    let wallpaper = app_file(rig.roots.user_dir.as_ref(), KIOSK, "wallpaper.txt");
    assert_eq!(read(&wallpaper).as_deref(), Some("new"));
}

/// Vendor, admin and user files for the display namespace.
fn display_files(roots: &Roots) {
    let vendor = shared_file(roots.vendor_dir.as_ref(), DISPLAY);
    put(&vendor, "brightness = 40\npanel = \"lcd\"\n");
    let admin = shared_file(roots.admin_dir.as_ref(), DISPLAY);
    put(&admin, "mode = \"night\"\n");
    let user = shared_file(roots.user_dir.as_ref(), DISPLAY);
    put(
        &user,
        "brightness = 99\nmode = \"user\"\npanel = \"user\"\n",
    );
}

/// The session daemon refuses a privileged namespace.
#[tokio::test(flavor = "multi_thread")]
async fn the_session_daemon_refuses_a_privileged_namespace() {
    let Some(rig) = rig().await else {
        return;
    };
    display_files(&rig.roots);
    let roots = &rig.roots;
    let files = [
        shared_file(roots.admin_dir.as_ref(), DISPLAY),
        shared_file(roots.user_dir.as_ref(), DISPLAY),
    ];
    let before: Vec<_> = files.iter().map(|file| read(file)).collect();
    let proxy = caller(&rig.bus).await;

    let subscribed = hold_as(&proxy, TOOL, DISPLAY).await;
    assert_eq!(error_name(subscribed), UNKNOWN_OBJECT);
    let applied = apply(&proxy, DISPLAY, &[("brightness", true, "70")]).await;
    assert_eq!(error_name(applied), UNKNOWN_OBJECT);
    let written = write_asset(&proxy, rig.tmp.path(), DISPLAY, "calibration", "x").await;
    assert_eq!(error_name(written), UNKNOWN_OBJECT);
    let cleared = clear_asset(&proxy, DISPLAY, "calibration").await;
    assert_eq!(error_name(cleared), UNKNOWN_OBJECT);

    let after: Vec<_> = files.iter().map(|file| read(file)).collect();
    assert_eq!(after, before);
}

/// A client with only a session link can't open a privileged namespace.
#[tokio::test(flavor = "multi_thread")]
async fn a_privileged_open_without_a_system_link_is_unreachable() {
    let Some(rig) = rig().await else {
        return;
    };
    let (source, mut link) = linked(&rig.bus).await;
    let e = try_open(&source, &mut link, DISPLAY).await.unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Unreachable(Daemon::System), "{e}");
}
