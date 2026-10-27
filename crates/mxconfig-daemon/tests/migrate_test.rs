//! Schema changes the daemon sees, and typed clients built from older or newer schemas.

mod common;

use std::time::Duration;

use common::{BUS_NAME, Bus, DESKTOP, NAMESPACE, ZbusLink, apply, connect, linked, open};
use common::{opened, recorded, released, rename_edge, serve, served, try_open, user_file};
use common::{within, written};
use mxconfig::host::Roots;
use mxconfig::{
    Edit, Error, ErrorKind, Namespace, Outcome, Settings, Source, Table, Ticket, Value,
};
use mxconfig_daemon::Service;

/// The player client an older build is generated from.
mod v1 {
    mxconfig::client!("tests/schemas/player.v1.toml");
}

/// The player client a newer build is generated from.
mod v2 {
    mxconfig::client!("tests/schemas/player.v2.toml");
}

const V1: &str = include_str!("schemas/player.v1.toml");
const V2: &str = include_str!("schemas/player.v2.toml");

/// An app namespace no schema is installed for, so apps register it.
const APP: &str = "org.example.newapp";

/// Whether `outcomes` say `namespace`'s handle detached.
fn detached(outcomes: &[Outcome], namespace: &str) -> bool {
    let gone = |o: &Outcome| matches!(o, Outcome::Detached { namespace: n, .. } if n == namespace);
    outcomes.iter().any(gone)
}

/// Open typed client `T` through the daemon, or the error it got.
async fn try_typed<T: Namespace>(source: &Source, link: &mut ZbusLink) -> Result<T, Error> {
    let ticket = source.request::<T>();
    link.settle().await;
    opened(source, ticket)
}

/// Open typed client `T` through the daemon.
async fn open_typed<T: Namespace>(source: &Source, link: &mut ZbusLink) -> T {
    try_typed(source, link).await.unwrap()
}

/// Wait for a write started as `ticket` to land. Returns its result.
async fn landed(
    source: &Source,
    link: &mut ZbusLink,
    ticket: Result<Ticket<mxconfig::Write>, Error>,
) -> Result<(), Error> {
    let ticket = ticket?;
    link.settle().await;
    written(source, ticket)
}

/// Write `edits` through the daemon and wait for them to land.
async fn write(settings: &Settings, link: &mut ZbusLink, edits: &[Edit]) {
    landed(settings.source(), link, settings.write(edits))
        .await
        .unwrap();
}

/// Drop `handle`, let the source unsubscribe, and wait until `left` subscribers remain.
async fn release<T>(
    handle: T,
    link: &mut ZbusLink,
    service: &Service,
    namespace: &str,
    left: usize,
) {
    drop(handle);
    link.settle().await;
    released(service, namespace, left).await;
}

/// Install `schema` for [`NAMESPACE`] in the daemon's schema dir.
fn install(roots: &Roots, schema: &str) {
    common::install(roots, NAMESPACE, schema);
}

/// v2, then `audio.level` renamed to `audio.gain`.
fn v3() -> String {
    V2.replace("version = 2", "version = 3")
        .replace("[keys.audio.level]", "[keys.audio.gain]")
        + &rename_edge(2, 3, "audio.level", "audio.gain")
}

/// The daemon's user file for `namespace`, parsed.
fn on_disk(roots: &Roots, namespace: &str) -> Table {
    let text = std::fs::read_to_string(user_file(roots, namespace, "config.toml")).unwrap();
    text.parse().unwrap()
}

/// The value at `group.key` in a parsed file.
fn at<'a>(table: &'a Table, group: &str, key: &str) -> Option<&'a Value> {
    table.get(group)?.get(key)
}

/// `mxconfigd` as its own process over `roots`, as on a device. Killed on drop.
struct Process(std::process::Child);

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Start `mxconfigd` over `roots` on `bus`, and wait until it owns its name.
async fn spawn(bus: &Bus, roots: &Roots) -> Process {
    let dir = |dir: &Option<std::path::PathBuf>| dir.clone().unwrap().into_os_string();
    let child = std::process::Command::new(env!("CARGO_BIN_EXE_mxconfigd"))
        .env("DBUS_SESSION_BUS_ADDRESS", &bus.address)
        .env("MXCONFIG_SCHEMA_DIR", &roots.schema_dirs[0])
        .env("MXCONFIG_VENDOR_DIR", dir(&roots.vendor_dir))
        .env("MXCONFIG_ADMIN_DIR", dir(&roots.admin_dir))
        .env("MXCONFIG_USER_DIR", dir(&roots.user_dir))
        .env("MXCONFIG_MANIFEST_DIR", dir(&roots.manifest_dir))
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let process = Process(child);
    let conn = connect(bus, None).await;
    let dbus = zbus::fdo::DBusProxy::new(&conn).await.unwrap();
    let name = zbus::names::BusName::try_from(BUS_NAME).unwrap();
    let owned = async {
        while !dbus.name_has_owner(name.clone()).await.unwrap() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    within(owned).await;
    process
}

/// A v1 build that has written `audio.volume = 35` and `audio.delay = "250"`.
async fn v1_with_values(bus: &Bus, roots: &Roots) -> (Source, ZbusLink, v1::Player) {
    install(roots, V1);
    let (source, mut link) = linked(bus).await;
    let player = open_typed::<v1::Player>(&source, &mut link).await;
    let audio = player.audio();
    landed(&source, &mut link, audio.set_volume(35))
        .await
        .unwrap();
    landed(&source, &mut link, audio.set_delay("250"))
        .await
        .unwrap();
    landed(&source, &mut link, audio.set_eq("rock"))
        .await
        .unwrap();
    assert_eq!(recorded(roots, NAMESPACE), (1, vec![]), "stamped on write");
    (source, link, player)
}

/// A client opening through the daemon gets the values the daemon migrated.
#[tokio::test(flavor = "multi_thread")]
async fn a_client_opening_through_the_daemon_gets_migrated_values() {
    let Some((bus, _server, roots, _tmp)) = served().await else {
        return;
    };
    // A v1 build wrote this file, and v3 is installed now.
    let file = "[audio]\nvolume = 35\ndelay = \"250\"\neq = \"rock\"\n";
    common::put(&user_file(&roots, NAMESPACE, "config.toml"), file);
    common::put(
        &user_file(&roots, NAMESPACE, "config.toml.state"),
        "version = 1\n",
    );
    install(&roots, &v3());

    let (source, mut link) = linked(&bus).await;
    let settings = open(&source, &mut link, NAMESPACE).await;
    assert_eq!(settings.describe().version(), 3);
    assert_eq!(settings.get("audio.gain"), Some(Value::Integer(35)));
    assert_eq!(settings.get("audio.delay"), Some(Value::Integer(250)));
    assert_eq!(settings.get("audio.balance"), Some(Value::Integer(0)));
    assert_eq!(settings.get("audio.eq"), None);

    let file = on_disk(&roots, NAMESPACE);
    assert_eq!(at(&file, "audio", "gain"), Some(&Value::Integer(35)));
    assert_eq!(at(&file, "audio", "volume"), None, "{file:?}");
    assert_eq!(recorded(&roots, NAMESPACE), (3, vec![(1, 2), (2, 3)]));
}

/// A subscribed namespace keeps its schema through an update, and writes still land.
#[tokio::test(flavor = "multi_thread")]
async fn an_update_waits_while_a_client_is_subscribed() {
    let Some((bus, _server, roots, _tmp)) = served().await else {
        return;
    };
    let (source, mut link, player) = v1_with_values(&bus, &roots).await;
    let before = on_disk(&roots, NAMESPACE);

    install(&roots, V2);
    // Nothing reaches the client, and the file stays as v1 wrote it.
    let quiet = tokio::time::timeout(Duration::from_millis(300), link.next()).await;
    assert!(quiet.is_err(), "no signal for a held namespace");
    assert!(source.outcomes().is_empty());
    assert_eq!(on_disk(&roots, NAMESPACE), before);
    assert_eq!(recorded(&roots, NAMESPACE), (1, vec![]));

    // The v1 build reads and writes as before.
    let audio = player.audio();
    assert_eq!(audio.volume(), 35);
    assert_eq!(audio.delay(), "250");
    landed(&source, &mut link, audio.set_volume(36))
        .await
        .unwrap();
    assert_eq!(audio.volume(), 36);
    assert_eq!(
        at(&on_disk(&roots, NAMESPACE), "audio", "volume"),
        Some(&Value::Integer(36))
    );
}

/// The last subscriber leaving lets the next open migrate, across every kind of op.
#[tokio::test(flavor = "multi_thread")]
async fn the_next_open_after_the_last_subscriber_migrates() {
    let Some((bus, service, roots, _tmp)) = served().await else {
        return;
    };
    let (source, mut link, player) = v1_with_values(&bus, &roots).await;
    let output = player.audio().set_output(v1::AudioOutput::Hdmi);
    landed(&source, &mut link, output).await.unwrap();
    install(&roots, V2);
    release(player, &mut link, &service, NAMESPACE, 0).await;

    let player = open_typed::<v2::Player>(&source, &mut link).await;
    let audio = player.audio();
    assert_eq!(audio.level(), 35, "renamed");
    assert_eq!(audio.delay(), 250, "retyped");
    assert_eq!(audio.balance(), 0, "added at its default");
    assert!(!audio.mute());
    // `hdmi` is no longer a value, so the getter reads its default.
    assert_eq!(audio.output(), v2::AudioOutput::Speakers);
    assert_eq!(recorded(&roots, NAMESPACE), (2, vec![(1, 2)]));
    let file = on_disk(&roots, NAMESPACE);
    let kept = Some(&Value::String("rock".into()));
    assert_eq!(at(&file, "audio", "eq"), kept, "retired, kept until pruned");
    assert_eq!(at(&file, "audio", "level"), Some(&Value::Integer(35)));

    // A write after the update lands under the new schema.
    landed(&source, &mut link, audio.set_level(40))
        .await
        .unwrap();
    assert_eq!(
        at(&on_disk(&roots, NAMESPACE), "audio", "level"),
        Some(&Value::Integer(40))
    );
}

/// Each update is taken at the next open, and the file moves step by step.
#[tokio::test(flavor = "multi_thread")]
async fn successive_updates_migrate_the_daemon_file_step_by_step() {
    let Some((bus, service, roots, _tmp)) = served().await else {
        return;
    };
    install(&roots, V1);
    let (source, mut link) = linked(&bus).await;
    let settings = open(&source, &mut link, NAMESPACE).await;
    write(&settings, &mut link, &[Edit::set("audio.volume", 35)]).await;

    install(&roots, V2);
    release(settings, &mut link, &service, NAMESPACE, 0).await;
    let settings = open(&source, &mut link, NAMESPACE).await;
    assert_eq!(settings.describe().version(), 2);
    assert_eq!(settings.get("audio.level"), Some(Value::Integer(35)));
    assert_eq!(recorded(&roots, NAMESPACE), (2, vec![(1, 2)]));

    install(&roots, &v3());
    release(settings, &mut link, &service, NAMESPACE, 0).await;
    let settings = open(&source, &mut link, NAMESPACE).await;
    assert_eq!(settings.describe().version(), 3);
    assert_eq!(settings.get("audio.gain"), Some(Value::Integer(35)));
    assert_eq!(settings.get("audio.level"), None);
    assert_eq!(recorded(&roots, NAMESPACE), (3, vec![(1, 2), (2, 3)]));
}

/// An older installed schema moves the file back down at the next open.
#[tokio::test(flavor = "multi_thread")]
async fn an_older_installed_schema_moves_the_daemon_file_back() {
    let Some((bus, service, roots, _tmp)) = served().await else {
        return;
    };
    install(&roots, V1);
    let (source, mut link) = linked(&bus).await;
    let settings = open(&source, &mut link, NAMESPACE).await;
    write(&settings, &mut link, &[Edit::set("audio.volume", 20)]).await;
    install(&roots, &v3());
    release(settings, &mut link, &service, NAMESPACE, 0).await;
    let settings = open(&source, &mut link, NAMESPACE).await;
    assert_eq!(settings.get("audio.gain"), Some(Value::Integer(20)));

    install(&roots, V1);
    release(settings, &mut link, &service, NAMESPACE, 0).await;
    let settings = open(&source, &mut link, NAMESPACE).await;
    assert_eq!(settings.describe().version(), 1);
    assert_eq!(settings.get("audio.volume"), Some(Value::Integer(20)));
    assert_eq!(recorded(&roots, NAMESPACE), (1, vec![]));
}

/// A restart onto a newer schema keeps serving a running build its own app namespace.
#[tokio::test(flavor = "multi_thread")]
async fn a_restart_keeps_serving_an_app_its_own_schema() {
    let (roots, _tmp) = common::fixture();
    let Some(bus) = common::start_bus() else {
        return;
    };
    let old = spawn(&bus, &roots).await;
    let (source, mut link, player) = v1_with_values(&bus, &roots).await;

    // The daemon dies, v2 is installed, and a new daemon starts.
    drop(old);
    install(&roots, V2);
    let (_new, service) = serve(&bus, &roots, mxconfig::Bus::Session).await;
    let resubscribed = async {
        while service.subscribers(NAMESPACE) == 0 {
            assert!(link.next().await, "the bus stayed up");
        }
    };
    within(resubscribed).await;
    link.settle().await;
    assert!(!detached(&source.outcomes(), NAMESPACE));

    // The app's own schema is trusted, so its file stays on v1 and every key works.
    let audio = player.audio();
    assert_eq!(audio.volume(), 35);
    assert_eq!(audio.delay(), "250");
    landed(&source, &mut link, audio.set_volume(36))
        .await
        .unwrap();
    assert_eq!(audio.volume(), 36);
    assert_eq!(recorded(&roots, NAMESPACE), (1, vec![]));

    // Once it goes, the next open takes the installed v2.
    drop(audio);
    release(player, &mut link, &service, NAMESPACE, 0).await;
    let player = open_typed::<v2::Player>(&source, &mut link).await;
    assert_eq!(player.audio().level(), 36);
    assert_eq!(recorded(&roots, NAMESPACE), (2, vec![(1, 2)]));
}

/// The shared namespace at v2: `accent` renamed to `color`.
fn desktop_v2() -> String {
    common::DESKTOP_SCHEMA
        .replace("version = 1", "version = 2")
        .replace("[keys.accent]", "[keys.color]")
        + &rename_edge(1, 2, "accent", "color")
}

/// A client whose keys a newer shared schema renamed stays on its defaults, and only reads.
#[tokio::test(flavor = "multi_thread")]
async fn a_client_of_an_older_shared_schema_stays_on_its_defaults() {
    let (roots, _tmp) = common::fixture();
    let Some(bus) = common::start_bus() else {
        return;
    };
    common::install(&roots, DESKTOP, &desktop_v2());
    let _daemon = serve(&bus, &roots, mxconfig::Bus::Session).await;

    // The daemon serves it the newer schema, and the client judges it.
    let (source, mut link) = linked(&bus).await;
    source.embed(DESKTOP, common::DESKTOP_SCHEMA);
    let settings = try_open(&source, &mut link, DESKTOP).await.unwrap();
    assert!(settings.on_defaults());
    assert_eq!(settings.other_version(), Some(2));
    let refused = settings.write(&[Edit::set("accent", "red")]).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::VersionConflict, "{refused}");

    // A client reading none of the renamed keys reads the daemon's values.
    let (source, mut link) = linked(&bus).await;
    source.embed(
        DESKTOP,
        &format!("namespace = \"{DESKTOP}\"\nversion = 1\n"),
    );
    let settings = open(&source, &mut link, DESKTOP).await;
    assert_eq!(settings.other_version(), Some(2));
}

/// A newer build is refused while an older one holds the namespace, and opens once it goes.
#[tokio::test(flavor = "multi_thread")]
async fn a_newer_build_is_refused_while_an_older_one_holds() {
    let Some((bus, service, roots, _tmp)) = served().await else {
        return;
    };
    let (_old_source, mut old_link, older) = v1_with_values(&bus, &roots).await;
    install(&roots, V2);

    let (new_source, mut new_link) = linked(&bus).await;
    let refused = try_typed::<v2::Player>(&new_source, &mut new_link).await;
    let Err(refused) = refused else {
        panic!("refused")
    };
    assert_eq!(refused.kind(), ErrorKind::VersionConflict, "{refused}");
    assert!(refused.to_string().contains("v1"), "{refused}");

    // The older build is untouched and keeps working.
    assert_eq!(older.audio().volume(), 35);
    landed(
        older.settings().source(),
        &mut old_link,
        older.audio().set_mute(true),
    )
    .await
    .unwrap();
    assert_eq!(recorded(&roots, NAMESPACE), (1, vec![]));

    // Once it goes, the newer build opens and migrates.
    release(older, &mut old_link, &service, NAMESPACE, 0).await;
    let newer = open_typed::<v2::Player>(&new_source, &mut new_link).await;
    assert_eq!(newer.audio().level(), 35);
    assert!(newer.audio().mute());
    assert_eq!(recorded(&roots, NAMESPACE), (2, vec![(1, 2)]));
}

/// A local process holding v1 keeps the daemon from opening the namespace at v2.
#[tokio::test(flavor = "multi_thread")]
async fn a_local_holder_keeps_the_daemon_off_another_version() {
    let Some((bus, _service, roots, _tmp)) = served().await else {
        return;
    };
    install(&roots, V1);
    let local = Source::with_roots(roots.clone());
    let held = local
        .open_schema(mxconfig::Schema::parse(V1).unwrap())
        .unwrap();
    held.write(&[Edit::set("audio.volume", 20)]).unwrap();
    install(&roots, V2);

    // A tool's write opens the installed v2, which the local v1 holder refuses.
    let proxy = common::proxy(&bus).await;
    let edits = [("audio.level", true, "30")];
    let refused = apply(&proxy, NAMESPACE, &edits).await.unwrap_err();
    assert!(refused.to_string().contains("VersionConflict"), "{refused}");
    assert_eq!(recorded(&roots, NAMESPACE), (1, vec![]));

    // A v2 client is refused too, and opens once the local holder goes.
    let (source, mut link) = linked(&bus).await;
    let refused = try_typed::<v2::Player>(&source, &mut link).await;
    let Err(refused) = refused else {
        panic!("refused")
    };
    assert_eq!(refused.kind(), ErrorKind::VersionConflict, "{refused}");
    drop(held);
    let player = open_typed::<v2::Player>(&source, &mut link).await;
    assert_eq!(player.audio().level(), 20);
}

/// A write without a subscription opens the namespace afresh, under the current schema.
#[tokio::test(flavor = "multi_thread")]
async fn an_unsubscribed_write_uses_the_current_schema() {
    let Some((bus, _server, roots, _tmp)) = served().await else {
        return;
    };
    install(&roots, V1);
    let proxy = common::proxy(&bus).await;
    let volume = [("audio.volume", true, "30")];
    apply(&proxy, NAMESPACE, &volume).await.unwrap();

    install(&roots, V2);
    let old = apply(&proxy, NAMESPACE, &volume).await.unwrap_err();
    assert!(old.to_string().contains("InvalidArgs"), "{old}");
    let level = [("audio.level", true, "30")];
    let (_, values) = apply(&proxy, NAMESPACE, &level).await.unwrap();
    assert!(
        values.contains_key("audio.balance"),
        "the v2 values: {values:?}"
    );
    assert_eq!(recorded(&roots, NAMESPACE), (2, vec![(1, 2)]));
}

/// The app's own schema at `version`: v2 renames `level` to `depth`.
fn app(version: u32) -> String {
    let key = if version == 1 { "level" } else { "depth" };
    let mut text = format!(
        "namespace = \"{APP}\"\nversion = {version}\nbackend = \"daemon\"\n\n[keys.{key}]\ntype = \"integer\"\ndefault = 3\nlabel = \"L\"\ndescription = \"d\"\n"
    );
    if version > 1 {
        text += &rename_edge(1, 2, "level", "depth");
    }
    text
}

/// A client that carries the app's schema at `version` to the daemon.
async fn carrying(bus: &Bus, version: u32) -> (Source, ZbusLink) {
    let (source, link) = linked(bus).await;
    source.embed(APP, &app(version));
    (source, link)
}

/// A newer registration is refused while an older build holds, then migrates the file.
#[tokio::test(flavor = "multi_thread")]
async fn a_newer_registered_schema_migrates_the_daemon_file_once_released() {
    let Some((bus, service, roots, _tmp)) = served().await else {
        return;
    };
    let (old, mut old_link) = carrying(&bus, 1).await;
    let older = open(&old, &mut old_link, APP).await;
    write(&older, &mut old_link, &[Edit::set("level", 4)]).await;

    // A newer build registers v2, but the older one holds v1.
    let (new, mut new_link) = carrying(&bus, 2).await;
    let refused = try_open(&new, &mut new_link, APP).await.unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::VersionConflict, "{refused}");
    assert_eq!(older.get("level"), Some(Value::Integer(4)), "untouched");
    assert_eq!(recorded(&roots, APP), (1, vec![]));

    release(older, &mut old_link, &service, APP, 0).await;
    let newer = open(&new, &mut new_link, APP).await;
    assert_eq!(newer.get("depth"), Some(Value::Integer(4)));
    assert_eq!(on_disk(&roots, APP).get("depth"), Some(&Value::Integer(4)));
    assert_eq!(recorded(&roots, APP), (2, vec![(1, 2)]));
}

/// An older build is refused while a newer one holds, and its own schema is used once it goes.
#[tokio::test(flavor = "multi_thread")]
async fn an_older_build_is_refused_while_a_newer_one_holds() {
    let Some((bus, service, roots, _tmp)) = served().await else {
        return;
    };
    // v1 wrote first, so v2 records the step it can be undone by.
    let (old, mut old_link) = carrying(&bus, 1).await;
    let older = open(&old, &mut old_link, APP).await;
    write(&older, &mut old_link, &[Edit::set("level", 4)]).await;
    release(older, &mut old_link, &service, APP, 0).await;
    let (new, mut new_link) = carrying(&bus, 2).await;
    let newer = open(&new, &mut new_link, APP).await;
    write(&newer, &mut new_link, &[Edit::set("depth", 7)]).await;

    let refused = try_open(&old, &mut old_link, APP).await.unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::VersionConflict, "{refused}");
    write(&newer, &mut new_link, &[Edit::set("depth", 8)]).await;
    assert_eq!(recorded(&roots, APP), (2, vec![(1, 2)]));

    // A rollback: with the newer build gone, the older one opens on its own schema.
    release(newer, &mut new_link, &service, APP, 0).await;
    let older = open(&old, &mut old_link, APP).await;
    assert_eq!(older.describe().version(), 1);
    assert_eq!(older.get("level"), Some(Value::Integer(8)));
    assert_eq!(recorded(&roots, APP), (1, vec![]));
}
