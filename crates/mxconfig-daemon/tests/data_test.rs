//! Checks the install files in `data/` against the constants in code.

use std::path::Path;

use mxconfig::Schema;
use mxconfig::host::protocol::{BUS_NAME, WRITE_ACTION};
use mxconfig::host::{ADMIN_BASE, Home};

/// The text of `data/<name>`.
fn data(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../data")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The daemon binary's file name.
fn daemon() -> String {
    let bin = Path::new(env!("CARGO_BIN_EXE_mxconfigd"));
    bin.file_name().unwrap().to_string_lossy().into_owned()
}

#[test]
fn the_install_files_name_the_bus_and_the_polkit_action() {
    let lines = |name: &str| data(name).lines().map(str::to_string).collect::<Vec<_>>();
    for unit in ["mxconfigd.service", "mxconfigd-privileged.service"] {
        assert!(
            lines(unit).contains(&format!("BusName={BUS_NAME}")),
            "{unit}"
        );
    }
    // Each activation file starts its own daemon through its own unit.
    let activations = [
        (
            "org.mechanix.MxConfig.session.service",
            "mxconfigd.service",
            false,
        ),
        (
            "org.mechanix.MxConfig.system.service",
            "mxconfigd-privileged.service",
            true,
        ),
    ];
    for (file, unit, privileged) in activations {
        let lines = lines(file);
        assert!(lines.contains(&format!("Name={BUS_NAME}")), "{file}");
        assert!(lines.contains(&format!("SystemdService={unit}")), "{file}");
        let exec = lines.iter().find(|l| l.starts_with("Exec=")).unwrap();
        assert_eq!(exec.ends_with("--privileged"), privileged, "{file}");
        assert!(
            exec.starts_with(&format!("Exec=/usr/bin/{}", daemon())),
            "{file}"
        );
    }
    for unit in ["mxconfigd.service", "mxconfigd-privileged.service"] {
        let start = format!("ExecStart=/usr/bin/{}", daemon());
        assert!(lines(unit).iter().any(|l| l.starts_with(&start)), "{unit}");
    }
    let conf = data("org.mechanix.MxConfig.conf");
    assert!(conf.contains(&format!("<allow own=\"{BUS_NAME}\"/>")));
    assert!(conf.contains(&format!("<allow send_destination=\"{BUS_NAME}\"/>")));
    let action = format!("<action id=\"{WRITE_ACTION}\">");
    assert!(data("org.mechanix.mxconfig.policy").contains(&action));
}

#[test]
fn tmpfiles_creates_the_shared_home_of_the_admin_layer() {
    let shared = Schema::parse("namespace = \"org.example.desktop.theme\"\nversion = 1\n").unwrap();
    let tmpfiles = data("mxconfig.tmpfiles.conf");
    let bound = Home::of(Path::new(ADMIN_BASE), &shared).bound;
    let line = format!("d {} 0755 root root -", bound.display());
    assert!(tmpfiles.lines().any(|l| l == line), "{line}");
}
