//! Access control: who the caller is, then its manifest.

use mxconfig::Class;
use mxconfig::host::{Roots, Sandbox};
use mxconfig::{Access, Manifest};

/// What the daemon knows about a caller: its sandbox, or `None` if unconfined.
pub(crate) type Caller = Option<Sandbox>;

/// The app a caller is: a sandbox's own id, else the one it claims. `None` if neither.
pub(crate) fn app<'a>(caller: Option<&'a Sandbox>, claimed: &'a str) -> Option<&'a str> {
    match caller {
        Some(sandbox) => sandbox.app.as_deref(),
        None => Some(claimed).filter(|claimed| !claimed.is_empty()),
    }
}

/// What `app` may reach, with its manifest under `roots` read once.
pub(crate) fn grants<'a>(roots: &Roots, app: Option<&'a str>) -> Grants<'a> {
    let file = app.and_then(|app| roots.manifest_file(app));
    let manifest = file.and_then(|file| Manifest::from_file(&file).ok());
    Grants { app, manifest }
}

/// An app and its manifest.
pub(crate) struct Grants<'a> {
    app: Option<&'a str>,
    manifest: Option<Manifest>,
}

impl Grants<'_> {
    /// Whether `namespace` may be used with `access`: its own, or what its manifest grants.
    pub(crate) fn allow(&self, namespace: &str, access: Access) -> bool {
        let Some(app) = self.app else {
            return false;
        };
        if Class::is_app(namespace) && app == namespace {
            return true;
        }
        let manifest = self.manifest.as_ref();
        manifest.is_some_and(|manifest| manifest.check(namespace, access).is_ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_app_reaches_its_own_namespace_and_what_its_manifest_declares() {
        let dir = tempfile::tempdir().unwrap();
        let roots = Roots {
            manifest_dir: Some(dir.path().to_path_buf()),
            ..Roots::default()
        };
        let allow = |app, namespace, access| grants(&roots, app).allow(namespace, access);
        let player = Some("org.example.player");
        assert!(allow(player, "org.example.player", Access::ReadWrite));
        let other = Some("org.example.other");
        assert!(!allow(other, "org.example.player", Access::Read));
        let theme = "org.example.desktop.theme";
        assert!(!allow(player, theme, Access::Read));

        let manifest = "client = \"org.example.player\"\n[[settings]]\nnamespace = \"org.example.desktop.theme\"\naccess = \"read\"\n";
        std::fs::write(dir.path().join("org.example.player.toml"), manifest).unwrap();
        assert!(allow(player, theme, Access::Read));
        assert!(!allow(player, theme, Access::ReadWrite));

        // Another app's namespace is reached only through a grant.
        let settings = "client = \"org.example.settings\"\n[[settings]]\nnamespace = \"org.example.player\"\naccess = \"read-write\"\n";
        std::fs::write(dir.path().join("org.example.settings.toml"), settings).unwrap();
        let settings = Some("org.example.settings");
        assert!(allow(settings, "org.example.player", Access::ReadWrite));

        // A caller with no id gets nothing.
        assert!(!allow(None, theme, Access::Read));
    }

    #[test]
    fn a_sandbox_is_its_own_id_and_others_are_what_they_claim() {
        let flatpak = Sandbox {
            app: Some("org.example.player".into()),
            ..Sandbox::default()
        };
        assert_eq!(
            app(Some(&flatpak), "org.example.other"),
            Some("org.example.player")
        );
        assert_eq!(app(Some(&Sandbox::default()), "org.example.other"), None);
        assert_eq!(app(None, "org.example.other"), Some("org.example.other"));
        assert_eq!(app(None, ""), None);
    }
}
