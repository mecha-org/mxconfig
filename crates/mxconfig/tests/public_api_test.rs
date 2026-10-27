//! The public API, checked against a committed snapshot. Needs a nightly toolchain.

use std::path::Path;

/// The committed listing of every public item.
const SNAPSHOT: &str = "tests/public-api.txt";

/// Fails when the public API changes. Set `UPDATE_PUBLIC_API=1` to accept a change.
#[test]
fn the_public_api_matches_its_snapshot() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let json = rustdoc_json::Builder::default()
        .toolchain("nightly")
        .manifest_path(&manifest)
        .silent(true)
        .build();
    let Ok(json) = json else {
        return eprintln!("no nightly rustdoc JSON; skipped");
    };
    let api = public_api::Builder::from_rustdoc_json(json)
        .omit_blanket_impls(true)
        .omit_auto_trait_impls(true)
        .omit_auto_derived_impls(true)
        .build()
        .unwrap();
    let listing = format!("{api}\n");
    let snapshot = Path::new(env!("CARGO_MANIFEST_DIR")).join(SNAPSHOT);
    if std::env::var_os("UPDATE_PUBLIC_API").is_some() {
        return std::fs::write(&snapshot, listing).unwrap();
    }
    let committed = std::fs::read_to_string(&snapshot).unwrap_or_default();
    assert!(
        committed == listing,
        "the public API changed; review it and rerun with UPDATE_PUBLIC_API=1"
    );
}
