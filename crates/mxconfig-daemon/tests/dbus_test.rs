//! Tests the daemon over a private bus.

mod common;

use std::time::Duration;

use common::{ACCESS_DENIED, BUS_NAME, DESKTOP, INTERFACE, NAMESPACE, OBJECT_PATH, TOOL};
use common::{apply, connect, error_name, hold_as, list, next_changed, proxy, proxy_named};
use common::{served, text, user_file};
use futures_util::StreamExt;
use mxconfig::host::protocol::{
    APPLY, ASSET_CHANGED, CHANGED, CLEAR_ASSET, LIST_NAMESPACES, SUBSCRIBE, WRITE_ASSET,
};

/// `ListNamespaces` lists what the app may access.
#[tokio::test]
async fn lists_namespaces() {
    let Some((bus, _server, _roots, _tmp)) = served().await else {
        return;
    };
    let proxy = proxy(&bus).await;

    assert_eq!(list(&proxy, TOOL).await.unwrap(), [DESKTOP, NAMESPACE]);
    assert_eq!(list(&proxy, NAMESPACE).await.unwrap(), [NAMESPACE]);
    assert_eq!(list(&proxy, "").await.unwrap(), Vec::<String>::new());
}

/// An app reaches its own namespace, and others only through its manifest. No app gets nothing.
#[tokio::test]
async fn an_app_reaches_what_its_manifest_grants() {
    let Some((bus, _server, roots, _tmp)) = served().await else {
        return;
    };
    let proxy = proxy(&bus).await;
    let volume = [("audio.volume", true, "35")];
    let accent = [("accent", true, "\"#ff0000\"")];

    // Nobody: refused everywhere, even the namespace its connection is named after.
    let anonymous = common::subscribe_as(&proxy, "", NAMESPACE).await;
    assert_eq!(error_name(anonymous), ACCESS_DENIED);
    let anonymous = common::apply_as(&proxy, "", NAMESPACE, &volume).await;
    assert_eq!(error_name(anonymous), ACCESS_DENIED);

    // Its own namespace needs no manifest. A shared one does.
    common::apply_as(&proxy, NAMESPACE, NAMESPACE, &volume)
        .await
        .unwrap();
    let ungranted = common::subscribe_as(&proxy, NAMESPACE, DESKTOP).await;
    assert_eq!(error_name(ungranted), ACCESS_DENIED);

    // A read grant reads but doesn't write.
    common::manifest(&roots, NAMESPACE, DESKTOP, "read");
    common::subscribe_as(&proxy, NAMESPACE, DESKTOP)
        .await
        .unwrap();
    let read_only = common::apply_as(&proxy, NAMESPACE, DESKTOP, &accent).await;
    assert_eq!(error_name(read_only), ACCESS_DENIED);

    common::manifest(&roots, NAMESPACE, DESKTOP, "read-write");
    common::apply_as(&proxy, NAMESPACE, DESKTOP, &accent)
        .await
        .unwrap();

    // Another app's namespace is not its own.
    let other = common::subscribe_as(&proxy, "org.example.other", NAMESPACE).await;
    assert_eq!(error_name(other), ACCESS_DENIED);
}

/// `Apply` writes the user file and the new value reads back.
#[tokio::test]
async fn apply_round_trips_through_the_user_file() {
    let Some((bus, _server, _roots, tmp)) = served().await else {
        return;
    };
    let proxy = proxy(&bus).await;

    let volume = [("audio.volume", true, "35")];
    let (revision, applied) = apply(&proxy, NAMESPACE, &volume).await.unwrap();
    assert!(revision >= 1);
    assert_eq!(
        text(applied.get("audio.volume").unwrap()),
        "35",
        "the reply has the values"
    );
    let all = common::subscribe(&proxy, NAMESPACE).await.unwrap();
    assert_eq!(all.get("audio.volume").map(String::as_str), Some("35"));

    // The daemon keeps app settings in the app's own directory.
    let user_file = tmp.path().join("user").join(NAMESPACE).join("config.toml");
    assert!(user_file.is_file(), "the write landed in the user dir");
}

/// `Subscribe` returns a snapshot, and a write sends `Changed`.
#[tokio::test]
async fn subscribe_returns_a_snapshot_then_changed() {
    let Some((bus, _server, _roots, _tmp)) = served().await else {
        return;
    };
    let proxy = proxy(&bus).await;

    let (_subscription, _, revision, snapshot, _) =
        hold_as(&proxy, NAMESPACE, NAMESPACE).await.unwrap();
    assert_eq!(revision, 0);
    assert_eq!(text(snapshot.get("audio.volume").unwrap()), "80");

    // Listen for the signal before writing, so it isn't missed.
    let mut signals = Box::pin(proxy.receive_signal("Changed").await.unwrap());
    let volume = [("audio.volume", true, "42")];
    apply(&proxy, NAMESPACE, &volume).await.unwrap();

    let (namespace, revision, values) = next_changed(&mut signals).await;
    assert_eq!(namespace, NAMESPACE);
    assert_eq!(text(values.get("audio.volume").unwrap()), "42");
    assert_eq!(
        text(values.get("audio.mute").unwrap()),
        "false",
        "every value, not just the change"
    );
    assert!(revision >= 1);
}

/// An unknown namespace is an error, not a panic.
#[tokio::test]
async fn unknown_namespace_is_an_error() {
    let Some((bus, _server, _roots, _tmp)) = served().await else {
        return;
    };
    let proxy = proxy(&bus).await;

    let result = hold_as(&proxy, TOOL, "org.example.other").await;
    assert!(result.is_err());
}

/// `Changed` is sent only to a namespace's subscribers.
#[tokio::test]
async fn changed_reaches_only_subscribers() {
    let Some((bus, _server, _roots, _tmp)) = served().await else {
        return;
    };
    let (writing, subscribing, reading) = (proxy(&bus).await, proxy(&bus).await, proxy(&bus).await);

    // One connection subscribes. The other only reads.
    let _subscription = hold_as(&subscribing, TOOL, DESKTOP).await.unwrap();
    let mut seen = Box::pin(subscribing.receive_signal("Changed").await.unwrap());
    let mut unseen = Box::pin(reading.receive_signal("Changed").await.unwrap());

    let accent = [("accent", true, "\"#ff0000\"")];
    apply(&writing, DESKTOP, &accent).await.unwrap();

    let (namespace, _, values) = next_changed(&mut seen).await;
    assert_eq!(namespace, DESKTOP);
    assert!(text(values.get("accent").unwrap()).contains("ff0000"));

    // The connection that did not subscribe gets nothing.
    let leaked = tokio::time::timeout(Duration::from_millis(500), unseen.next()).await;
    assert!(leaked.is_err(), "a non-subscriber must not receive Changed");
}

/// The interface name literal matches [`INTERFACE`].
#[tokio::test]
async fn served_interface_matches_the_constant() {
    let Some((bus, _server, _roots, _tmp)) = served().await else {
        return;
    };
    let client = connect(&bus, Some(NAMESPACE)).await;

    let introspectable = zbus::fdo::IntrospectableProxy::builder(&client)
        .destination(BUS_NAME)
        .unwrap()
        .path(OBJECT_PATH)
        .unwrap()
        .build()
        .await
        .unwrap();

    let served = introspectable.introspect().await.unwrap();
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../data/org.mechanix.MxConfig.xml"
    );
    let committed = std::fs::read_to_string(path).unwrap();
    assert_eq!(
        interface(&served),
        interface(&committed),
        "data/org.mechanix.MxConfig.xml no longer matches the daemon:\n{served}"
    );

    // The names clients send and listen for are the ones served.
    for method in [SUBSCRIBE, APPLY, WRITE_ASSET, CLEAR_ASSET, LIST_NAMESPACES] {
        assert!(
            served.contains(&format!("<method name=\"{method}\">")),
            "{method}"
        );
    }
    for signal in [CHANGED, ASSET_CHANGED] {
        assert!(
            served.contains(&format!("<signal name=\"{signal}\">")),
            "{signal}"
        );
    }
}

/// The interface from an introspection document, without comments or spacing.
fn interface(xml: &str) -> String {
    let open = format!("<interface name=\"{INTERFACE}\">");
    let start = xml.find(&open).expect("the interface is described");
    let end = start + xml[start..].find("</interface>").expect("and closed");
    let (mut text, mut rest) = (String::new(), &xml[start..end]);
    while let Some(at) = rest.find("<!--") {
        text.push_str(&rest[..at]);
        let close = rest[at..].find("-->").expect("a closed comment");
        rest = &rest[at + close + 3..];
    }
    text.push_str(rest);
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// An edit made outside the daemon reaches subscribers.
#[tokio::test]
async fn an_outside_edit_reaches_subscribers() {
    let Some((bus, _server, roots, _tmp)) = served().await else {
        return;
    };
    let proxy = proxy(&bus).await;
    let _subscription = hold_as(&proxy, NAMESPACE, NAMESPACE).await.unwrap();
    let mut signals = Box::pin(proxy.receive_signal("Changed").await.unwrap());

    let file = user_file(&roots, NAMESPACE, "config.toml");
    common::put(&file, "[audio]\nvolume = 7\n");

    let (_, _, values) = next_changed(&mut signals).await;
    assert_eq!(text(values.get("audio.volume").unwrap()), "7");
}

/// A subscription ends when its descriptor closes, however the client lets go of it.
#[tokio::test]
async fn closing_the_descriptor_ends_the_subscription() {
    let Some((bus, service, _roots, _tmp)) = served().await else {
        return;
    };
    let proxy = proxy(&bus).await;
    let first = hold_as(&proxy, NAMESPACE, NAMESPACE).await.unwrap();
    let second = hold_as(&proxy, NAMESPACE, NAMESPACE).await.unwrap();
    assert_eq!(service.subscribers(NAMESPACE), 2);
    drop(first);
    common::released(&service, NAMESPACE, 1).await;
    drop(second);
    common::released(&service, NAMESPACE, 0).await;
}

/// A held namespace keeps its schema until its last subscriber leaves.
#[tokio::test]
async fn a_held_namespace_keeps_its_schema_until_its_last_subscriber_leaves() {
    let Some((bus, service, roots, _tmp)) = served().await else {
        return;
    };
    let (first, second) = (
        proxy(&bus).await,
        proxy_named(&bus, "org.example.second").await,
    );
    let held_first = hold_as(&first, NAMESPACE, NAMESPACE).await.unwrap();
    let held_second = hold_as(&second, NAMESPACE, NAMESPACE).await.unwrap();
    let v2 = common::SCHEMA.replace("version = 1", "version = 2");
    common::install(&roots, NAMESPACE, &v2);
    // Each look subscribes for a moment, then lets go.
    let schema = |proxy: zbus::Proxy<'static>| async move {
        let (_, schema, ..) = hold_as(&proxy, NAMESPACE, NAMESPACE).await.unwrap();
        schema
    };
    assert!(
        schema(first.clone()).await.contains("version = 1"),
        "held by both"
    );

    drop(held_first);
    common::released(&service, NAMESPACE, 1).await;
    assert!(
        schema(second.clone()).await.contains("version = 1"),
        "still held"
    );
    drop(held_second);
    common::released(&service, NAMESPACE, 0).await;
    assert!(schema(first).await.contains("version = 2"));
}
