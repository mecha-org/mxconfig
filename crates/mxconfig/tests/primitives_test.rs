use mxconfig::ErrorKind;
use mxconfig::value;
use mxconfig::{Access, Manifest};

#[test]
fn value_round_trips_through_toml() {
    for src in [
        "35",
        "\"speakers\"",
        "true",
        "[1, 2, 3]",
        "1.5",
        "\"#7aa2f7\"",
    ] {
        let value = value::from_toml_str(src).expect(src);
        let rendered = value.to_string();
        let again = value::from_toml_str(&rendered).expect(&rendered);
        assert_eq!(value, again, "{src}");
    }

    let value = value::from_toml_str("35").unwrap();
    assert_eq!(value.as_integer(), Some(35));
    assert_eq!(value::from_toml_str("\"on\"").unwrap().as_str(), Some("on"));
    assert_eq!(value::from_toml_str("true").unwrap().as_bool(), Some(true));
    assert!(value::from_toml_str("not toml at all").is_err());
}

const MANIFEST: &str = r#"
client = "org.mechanix.demo"

[[settings]]
namespace = "org.mechanix.demo"
access = "read-write"

[[settings]]
namespace = "org.mechanix.desktop.theme"
access = "read"

[[settings]]
namespace = "org.mechanix.privileged.display"
access = "read-write"
temporary = true
"#;

#[test]
fn manifest_grants_and_refuses_access() {
    let manifest = Manifest::parse(MANIFEST).unwrap();
    assert_eq!(manifest.client, "org.mechanix.demo");

    assert!(
        manifest
            .check("org.mechanix.demo", Access::ReadWrite)
            .is_ok()
    );
    assert!(
        manifest
            .check("org.mechanix.desktop.theme", Access::Read)
            .is_ok()
    );
    assert!(
        manifest
            .check("org.mechanix.desktop.theme", Access::ReadWrite)
            .is_err()
    );
    assert!(
        manifest.check("org.other.thing", Access::Read).is_err(),
        "undeclared namespaces are invisible"
    );

    // Only the temporary grant must not ship.
    let diags = manifest.validate();
    assert_eq!(diags.len(), 1);
    assert!(
        diags[0].message().contains("privileged.display"),
        "{diags:?}"
    );
}

#[test]
fn manifest_allows_one_lasting_and_one_temporary_grant_per_namespace() {
    let entry = |access: &str, temporary: bool| {
        format!(
            "[[settings]]\nnamespace = \"a.b\"\naccess = \"{access}\"\ntemporary = {temporary}\n"
        )
    };
    let src = |entries: &[String]| format!("client = \"x\"\n{}", entries.concat());

    let manifest =
        Manifest::parse(&src(&[entry("read", false), entry("read-write", true)])).unwrap();
    assert!(manifest.check("a.b", Access::ReadWrite).is_ok());
    let diags = manifest.validate();
    assert_eq!(diags.len(), 1);
    assert!(diags[0].message().contains("must not ship"), "{diags:?}");

    assert!(Manifest::parse(&src(&[entry("read", false), entry("read-write", false)])).is_err());
}

#[test]
fn manifest_rejects_a_missing_client_or_bad_access() {
    assert!(Manifest::parse("[[settings]]\nnamespace = \"a.b\"\naccess = \"read\"").is_err());
    assert!(
        Manifest::parse(
            "client = \"x\"\n[[settings]]\nnamespace = \"a.b\"\naccess = \"sometimes\""
        )
        .is_err()
    );
}

#[test]
fn a_bus_parses_from_its_name() {
    assert_eq!(
        "session".parse::<mxconfig::Bus>().unwrap(),
        mxconfig::Bus::Session
    );
    assert_eq!(
        "system".parse::<mxconfig::Bus>().unwrap(),
        mxconfig::Bus::System
    );
    let wrong = "sideways".parse::<mxconfig::Bus>().unwrap_err();
    assert_eq!(wrong.kind(), ErrorKind::Invalid, "{wrong}");
}
