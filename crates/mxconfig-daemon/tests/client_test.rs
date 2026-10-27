//! Tests a `Source` against the daemon on a private bus, via `ZbusLink`.

mod common;

use common::{Bus, DESKTOP, NAMESPACE, ZbusLink, connect, hear, linked, open, served};
use common::{opened, try_open, user_file};
use mxconfig::host::Roots;
use mxconfig::{Detach, Edit, ErrorKind, Origin, Outcome, Settings, Source, Ticket, Value};

/// Install a vendor copy of the `layout` asset.
fn vendor_layout(roots: &Roots) {
    let vendor = roots.vendor_dir.as_ref().unwrap().join(NAMESPACE);
    common::put(&vendor.join("layout.yaml"), "vendor");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_backed_handle_reads_writes_and_hears_other_writers() {
    let Some((bus, _server, roots, _tmp)) = served().await else {
        return;
    };
    let (source, mut link) = linked(&bus).await;

    let settings = open(&source, &mut link, NAMESPACE).await;
    assert_eq!(settings.get("audio.volume"), Some(Value::Integer(80)));
    assert_eq!(
        settings.resolved().origin("audio.volume"),
        Some(&Origin::Daemon)
    );

    // A write goes to the daemon's user dir and is reported when done.
    let write = settings.write(&[Edit::set("audio.volume", 35)]).unwrap();
    link.settle().await;
    // The daemon sent `Changed` before replying, so the change is applied.
    let outcomes = source.outcomes();
    outcomes
        .iter()
        .find_map(|o| write.written(o))
        .unwrap()
        .unwrap();
    assert!(
        outcomes
            .iter()
            .any(|o| matches!(o, Outcome::Changed(c) if c.key == "audio.volume"))
    );
    assert_eq!(settings.get("audio.volume"), Some(Value::Integer(35)));
    assert!(user_file(&roots, NAMESPACE, "config.toml").is_file());

    // Another client's write arrives as a signal, then as an outcome.
    let (other, mut other_link) = linked(&bus).await;
    let theirs = open(&other, &mut other_link, NAMESPACE).await;
    theirs.write(&[Edit::set("audio.volume", 12)]).unwrap();
    other_link.settle().await;
    let twelve =
        |o: &Outcome| matches!(o, Outcome::Changed(c) if c.value == Some(Value::Integer(12)));
    hear(&source, &mut link, twelve).await;
    assert_eq!(settings.get("audio.volume"), Some(Value::Integer(12)));
}

/// Manifests only restrict sandboxed callers.
#[tokio::test(flavor = "multi_thread")]
async fn a_client_is_the_app_it_carries_or_the_one_it_names() {
    let Some((bus, _server, roots, _tmp)) = served().await else {
        return;
    };
    let source = Source::with_roots(Roots::default());
    let mut link = common::link(&bus, mxconfig::Bus::Session, &source).await;

    // With no app, a shared namespace is refused.
    let refused = try_open(&source, &mut link, DESKTOP).await.unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Denied, "{refused}");

    // Carrying one app namespace makes it that app, which needs a grant.
    source.embed(NAMESPACE, common::SCHEMA);
    let refused = try_open(&source, &mut link, DESKTOP).await.unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Denied, "{refused}");
    common::manifest(&roots, NAMESPACE, DESKTOP, "read");
    let settings = open(&source, &mut link, DESKTOP).await;
    assert_eq!(
        settings.get("accent"),
        Some(Value::String("#7aa2f7".into()))
    );
    drop(settings);

    // A named app is used for shared namespaces instead.
    source.set_app("org.example.unknown");
    let refused = try_open(&source, &mut link, DESKTOP).await.unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Denied, "{refused}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_app_brings_its_own_schema() {
    const APP: &str = r#"
namespace = "org.example.newapp"
version = 1
backend = "daemon"

[keys.level]
type = "integer"
default = 3
label = "Level"
description = "d"
"#;
    let Some((bus, _server, roots, _tmp)) = served().await else {
        return;
    };

    // Only the client carries this schema, and it prefers the daemon.
    let (source, mut link) = linked(&bus).await;
    source.embed("org.example.newapp", APP);
    let settings = open(&source, &mut link, "org.example.newapp").await;
    assert_eq!(settings.get("level"), Some(Value::Integer(3)));
    settings.write(&[Edit::set("level", 4)]).unwrap();
    link.settle().await;
    assert!(user_file(&roots, "org.example.newapp", "config.toml").is_file());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_namespace_fails_the_open() {
    let Some((bus, _server, _roots, _tmp)) = served().await else {
        return;
    };
    let (source, mut link) = linked(&bus).await;
    assert!(
        try_open(&source, &mut link, "org.example.nothing")
            .await
            .is_err()
    );
}

/// A daemon that never answers fails the call once its time runs out, instead of hanging.
#[tokio::test(flavor = "current_thread")]
async fn a_daemon_that_never_answers_times_out() {
    let Some(bus) = common::start_bus() else {
        return;
    };
    // It owns the daemon's name but serves nothing, as a wedged daemon would.
    let _silent = connect(&bus, Some(mxconfig::host::protocol::BUS_NAME)).await;
    let (source, mut link) = linked(&bus).await;
    link.settle().await;
    let ticket = source.request_namespace(DESKTOP);
    // Time passes while nothing else can happen.
    tokio::time::pause();
    link.settle().await;
    let e = opened(&source, ticket).unwrap_err();
    let session = mxconfig::Bus::Session;
    assert_eq!(e.kind(), ErrorKind::Unreachable(session), "{e}");
}

/// Run the link until an `AssetChanged` for `id` arrives, then read it.
async fn next_asset(
    source: &Source,
    link: &mut ZbusLink,
    settings: &Settings,
    id: &str,
) -> Option<String> {
    let moved = |o: &Outcome| matches!(o, Outcome::AssetChanged { id: i, .. } if i == id);
    hear(source, link, moved).await;
    settings.asset(id).map(|asset| asset.text().unwrap())
}

/// The daemon passes an asset's file, and a new one each time it changes.
#[tokio::test(flavor = "multi_thread")]
async fn assets_arrive_as_files_the_daemon_opened() {
    let Some((bus, _server, roots, _tmp)) = served().await else {
        return;
    };
    // An app namespace has its own directory in each layer.
    vendor_layout(&roots);
    let (source, mut link) = linked(&bus).await;

    let settings = open(&source, &mut link, NAMESPACE).await;
    let asset = settings.asset("layout").expect("served");
    assert_eq!(asset.origin, Origin::Daemon);
    assert_eq!(asset.text().unwrap(), "vendor");

    // A user copy overrides the vendor one. Written atomically.
    let user = user_file(&roots, NAMESPACE, "layout.yaml");
    common::put(&user, "mine");
    let text = next_asset(&source, &mut link, &settings, "layout").await;
    assert_eq!(text.as_deref(), Some("mine"));
    assert_eq!(asset.text().unwrap(), "vendor", "an old fd keeps its file");

    std::fs::remove_file(&user).unwrap();
    let text = next_asset(&source, &mut link, &settings, "layout").await;
    assert_eq!(text.as_deref(), Some("vendor"));
}

/// Asset writes reach the daemon's user layer, signal first, then reply.
#[tokio::test(flavor = "multi_thread")]
async fn asset_writes_go_as_files_the_daemon_copies() {
    let Some((bus, _server, roots, tmp)) = served().await else {
        return;
    };
    vendor_layout(&roots);
    let (source, mut link) = linked(&bus).await;
    let settings = open(&source, &mut link, NAMESPACE).await;
    let copy = user_file(&roots, NAMESPACE, "layout.yaml");

    // Each write sends `AssetChanged` before `Written`.
    let picked = tmp.path().join("picked.yaml");
    std::fs::write(&picked, "picked").unwrap();
    let from_file = std::os::fd::OwnedFd::from(std::fs::File::open(&picked).unwrap());
    let mut from_file = Some(from_file);
    for text in [Some("bytes"), Some("picked"), None] {
        let ticket = match text {
            Some("bytes") => settings.write_asset("layout", b"bytes"),
            Some(_) => settings.write_asset_from("layout", from_file.take().unwrap()),
            None => settings.clear_asset("layout"),
        };
        let ticket = ticket.unwrap();
        link.settle().await;
        let outcomes = source.outcomes();
        let landed = outcomes
            .iter()
            .position(|o| ticket.written(o).is_some())
            .unwrap();
        let moved = outcomes
            .iter()
            .position(|o| matches!(o, Outcome::AssetChanged { .. }));
        assert!(moved.unwrap() < landed, "{outcomes:?}");
        let written = ticket.written(&outcomes[landed]).unwrap();
        assert!(written.is_ok(), "{written:?}");
        assert_eq!(std::fs::read_to_string(&copy).ok().as_deref(), text);
        let now = settings.asset("layout").unwrap().text().unwrap();
        assert_eq!(now, text.unwrap_or("vendor"));
    }
}

/// Asset writes refuse pipes and files over the size limit.
#[tokio::test(flavor = "multi_thread")]
async fn an_asset_write_refuses_pipes_and_huge_files() {
    let Some((bus, _server, _roots, tmp)) = served().await else {
        return;
    };
    let conn = connect(&bus, None).await;
    let call = |fd: std::os::fd::OwnedFd| {
        let conn = conn.clone();
        async move {
            let fd = zbus::zvariant::OwnedFd::from(fd);
            conn.call_method(
                Some(common::BUS_NAME),
                common::OBJECT_PATH,
                Some(common::INTERFACE),
                "WriteAsset",
                &(fd, NAMESPACE, "layout", NAMESPACE),
            )
            .await
        }
    };
    // Keep the write end open, so reading the pipe would block forever.
    let (reader, _writer) = std::io::pipe().unwrap();
    let refused = call(reader.into()).await.unwrap_err().to_string();
    assert!(refused.contains("regular file"), "{refused}");

    let huge = std::fs::File::create(tmp.path().join("huge")).unwrap();
    huge.set_len(mxconfig::host::protocol::MAX_ASSET + 1)
        .unwrap();
    let huge = std::fs::File::open(tmp.path().join("huge")).unwrap();
    let refused = call(huge.into()).await.unwrap_err().to_string();
    assert!(refused.contains("at most"), "{refused}");
}

/// Closing the link releases the daemon's hold and detaches the handles.
#[tokio::test(flavor = "multi_thread")]
async fn closing_the_link_releases_the_hold_and_detaches() {
    let Some((bus, service, _roots, _tmp)) = served().await else {
        return;
    };
    let (source, mut link) = linked(&bus).await;
    let settings = open(&source, &mut link, NAMESPACE).await;
    assert_eq!(service.subscribers(NAMESPACE), 1);

    link.close();
    common::released(&service, NAMESPACE, 0).await;
    let closed = |o: &Outcome| {
        matches!(
            o,
            Outcome::Detached {
                cause: Detach::Closed,
                ..
            }
        )
    };
    assert!(source.outcomes().iter().any(closed));
    assert_eq!(
        settings.get("audio.volume"),
        Some(Value::Integer(80)),
        "kept"
    );
    let e = settings.write(&[Edit::set("audio.volume", 5)]).unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Detached);
}

/// Dropping a link only fails what waits on it. Nothing waits forever.
#[tokio::test(flavor = "multi_thread")]
async fn dropping_the_link_fails_what_waits_on_it() {
    let Some((bus, _server, _roots, _tmp)) = served().await else {
        return;
    };
    let (source, link) = linked(&bus).await;
    let ticket: Ticket<Settings> = source.request_namespace(NAMESPACE);
    drop(link);
    let e = opened(&source, ticket).unwrap_err();
    assert!(e.to_string().contains("cannot reach mxconfigd"), "{e}");
}

/// A stand-in for mxconfigd that takes `Subscribe` and never answers.
struct Stalled;

#[zbus::interface(name = "org.mechanix.MxConfig")]
impl Stalled {
    async fn subscribe(&self, namespace: String, schema: String) -> u64 {
        drop((namespace, schema));
        std::future::pending().await
    }
}

/// The stand-in's connection, a link to it, and an open it never answers.
type Parked = (zbus::Connection, Source, ZbusLink, Ticket<Settings>);

/// A source linked to a daemon that never answers, with an open sent to it.
async fn parked(bus: &Bus) -> Parked {
    let stalled = connect(bus, None).await;
    stalled
        .object_server()
        .at(common::OBJECT_PATH, Stalled)
        .await
        .unwrap();
    stalled.request_name(common::BUS_NAME).await.unwrap();
    let (source, mut link) = linked(bus).await;
    let ticket = source.request_namespace(NAMESPACE);
    // Sends the owner query and the open, and hears the owner.
    assert!(link.next().await);
    (stalled, source, link, ticket)
}

/// Closing the link while an open waits answers the open.
#[tokio::test(flavor = "multi_thread")]
async fn closing_the_link_while_an_open_waits_answers_it() {
    let Some(bus) = common::start_bus() else {
        return;
    };
    let (_stalled, source, link, ticket) = parked(&bus).await;
    link.close();
    assert!(opened(&source, ticket).is_err());
}

/// Dropping the link while an open waits answers the open too.
#[tokio::test(flavor = "multi_thread")]
async fn dropping_the_link_while_an_open_waits_answers_it() {
    let Some(bus) = common::start_bus() else {
        return;
    };
    let (_stalled, source, link, ticket) = parked(&bus).await;
    drop(link);
    assert!(opened(&source, ticket).is_err());
}
