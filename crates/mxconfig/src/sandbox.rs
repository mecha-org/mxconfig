//! Which sandbox a process runs in, from what Flatpak and Snap leave behind.

use std::fs;
use std::path::{Path, PathBuf};

use crate::paths::{self, APP, Roots, SCHEMAS};
use mxconfig_schema::Class;

/// The file Flatpak writes at the root of each app's sandbox.
const FLATPAK_INFO: &str = ".flatpak-info";

/// The start of a Snap app's security label.
const SNAP_LABEL: &str = "snap.";

/// Where Flatpak keeps each app's own directory, under the user's home.
const FLATPAK_APPS: &str = ".var/app";

/// Where a Flatpak app's installed files are mounted inside its sandbox.
const FLATPAK_APP_MOUNT: &str = "/app";

/// Where the host's directories are mounted inside a Flatpak sandbox.
pub(crate) const FLATPAK_HOST_MOUNT: &str = "/run/host";

/// A deploy's installed files, beside its `active` link.
const FLATPAK_FILES: &str = "files";

/// The link to a Flatpak app's current deploy.
const FLATPAK_ACTIVE: &str = "active";

/// An app's own data directory, under its instance.
const FLATPAK_DATA: &str = "data";

/// An app's own config directory, under its instance.
const FLATPAK_CONFIG: &str = "config";

/// The shared data directory of a Flatpak app's installed files.
const FLATPAK_SHARE: &str = "share";

/// A sandboxed process. Anything unknown about it stays `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sandbox {
    /// The Flatpak app id.
    pub app: Option<String>,
    /// The app's own directory, `~/.var/app/<app id>`.
    pub instance: Option<PathBuf>,
    /// The app's installed files, `/app` as the host sees it.
    pub files: Option<PathBuf>,
}

impl Sandbox {
    /// The sandbox process `pid` runs in, or `None` if it runs unconfined.
    pub fn of(pid: u32) -> Option<Sandbox> {
        Sandbox::at(&format!("/proc/{pid}"))
    }

    /// The sandbox this process runs in, if any.
    pub(crate) fn current() -> Option<Sandbox> {
        Sandbox::at("/proc/self")
    }

    /// The sandbox of the process at `proc`, its `/proc` directory.
    fn at(proc: &str) -> Option<Sandbox> {
        let root = Path::new(proc).join("root");
        // A process that can't be inspected counts as sandboxed.
        if fs::read_link(&root).is_err() {
            return Some(Sandbox::default());
        }
        if let Ok(info) = fs::read_to_string(root.join(FLATPAK_INFO)) {
            return Some(Sandbox::flatpak(&info));
        }
        let label = fs::read_to_string(Path::new(proc).join("attr/current")).unwrap_or_default();
        label.starts_with(SNAP_LABEL).then(Sandbox::default)
    }

    /// A Flatpak app, from the `.flatpak-info` Flatpak writes for it.
    fn flatpak(info: &str) -> Sandbox {
        let path = |key| {
            let path = entry(info, "Instance", key).map(PathBuf::from);
            path.filter(|p| p.is_absolute())
        };
        Sandbox {
            app: entry(info, "Application", "name"),
            instance: path("instance-path"),
            files: path("app-path"),
        }
    }

    /// The sandbox installed Flatpak app `app` runs in. Runs `flatpak`, so it blocks.
    pub fn installed(app: &str) -> Option<Sandbox> {
        // An app id can't be mistaken for an option.
        if !Class::is_app(app) {
            return None;
        }
        let output = std::process::Command::new("flatpak")
            .args(["info", "--show-location", app])
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        let location = String::from_utf8(output.stdout).ok()?;
        let location = Path::new(location.trim());
        if !output.status.success() || !location.is_absolute() {
            return None;
        }
        Some(Sandbox {
            app: Some(app.to_string()),
            instance: Some(paths::home_dir()?.join(FLATPAK_APPS).join(app)),
            files: Some(location.join(FLATPAK_FILES)),
        })
    }

    /// The Flatpak app to read `namespace` from, if `host` has no schema for it.
    pub fn instead_of(host: &Roots, namespace: &str) -> Option<Sandbox> {
        // The host's copy wins, so a native install keeps its own settings.
        if host.is_installed(namespace) {
            return None;
        }
        Sandbox::installed(namespace)
    }

    /// The roots of a Flatpak app's own namespace, as the host sees them.
    pub fn roots(&self) -> Option<Roots> {
        Some(Roots::flatpak(
            self.instance.as_deref()?,
            self.files.as_deref()?,
        ))
    }

    /// The same roots, as the app sees them from inside.
    pub(crate) fn roots_inside(&self) -> Option<Roots> {
        self.files.as_ref()?;
        Some(Roots::flatpak(
            self.instance.as_deref()?,
            Path::new(FLATPAK_APP_MOUNT),
        ))
    }
}

impl Roots {
    /// What a Flatpak app sees: its `instance` directory and its `files`.
    pub fn flatpak(instance: &Path, files: &Path) -> Roots {
        // Follow the deploy's `active` link, so an update is picked up.
        let branch = files.parent().and_then(Path::parent);
        let active = branch.map(|branch| branch.join(FLATPAK_ACTIVE).join(FLATPAK_FILES));
        let active = active.filter(|active| active.is_dir());
        let files = active.as_deref().unwrap_or(files);
        Roots {
            schema_dirs: vec![
                instance.join(FLATPAK_DATA).join(APP).join(SCHEMAS),
                files.join(FLATPAK_SHARE).join(APP).join(SCHEMAS),
            ],
            // `/app` is the app's `/usr`, so its `lib` is the vendor layer.
            vendor_dir: Some(files.join("lib")),
            user_dir: Some(instance.join(FLATPAK_CONFIG)),
            ..Roots::default()
        }
    }
}

/// The value of `key` in `section` of an ini-style `info`.
fn entry(info: &str, section: &str, key: &str) -> Option<String> {
    let mut inside = false;
    for line in info.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            inside = name == section;
        } else if inside
            && let Some((k, v)) = line.split_once('=')
            && k.trim() == key
        {
            return Some(v.trim().to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const INFO: &str = "[Application]\nname=org.example.player\nruntime=x\n\n[Instance]\nname=nope\ninstance-path=/home/u/.var/app/org.example.player\napp-path=/var/lib/flatpak/app/org.example.player/x86_64/stable/abc/files\n";

    #[test]
    fn a_flatpak_app_and_its_directories_come_from_its_info_file() {
        let sandbox = Sandbox::flatpak(INFO);
        assert_eq!(sandbox.app.as_deref(), Some("org.example.player"));
        let roots = sandbox.roots().expect("both paths are given");
        assert_eq!(
            roots.user_dir.as_deref(),
            Some(Path::new("/home/u/.var/app/org.example.player/config"))
        );
        let vendor =
            Path::new("/var/lib/flatpak/app/org.example.player/x86_64/stable/abc/files/lib");
        assert_eq!(roots.vendor_dir.as_deref(), Some(vendor));
        let inside = sandbox.roots_inside().unwrap();
        assert_eq!(
            inside.schema_dirs[1],
            Path::new("/app/share/mxconfig/schemas")
        );
        assert_eq!(inside.vendor_dir.as_deref(), Some(Path::new("/app/lib")));

        // A missing or relative path gives no directories.
        let partial = Sandbox::flatpak(
            "[Application]\nname=org.example.player\n[Instance]\napp-path=files\n",
        );
        assert!(partial.roots().is_none() && partial.roots_inside().is_none());
        assert_eq!(Sandbox::flatpak("[Instance]\nname=nope\n").app, None);
    }

    /// A fake `/proc/<pid>` with a root and, if given, a security label.
    fn proc(tmp: &Path, info: Option<&str>, label: Option<&str>) -> String {
        let (proc, root) = (tmp.join("proc"), tmp.join("rootfs"));
        fs::create_dir_all(proc.join("attr")).unwrap();
        fs::create_dir_all(&root).unwrap();
        std::os::unix::fs::symlink(&root, proc.join("root")).unwrap();
        if let Some(info) = info {
            fs::write(root.join(".flatpak-info"), info).unwrap();
        }
        if let Some(label) = label {
            fs::write(proc.join("attr/current"), label).unwrap();
        }
        proc.display().to_string()
    }

    #[test]
    fn flatpak_and_snap_processes_are_sandboxed_and_others_are_not() {
        let at = |info, label| {
            let tmp = tempfile::tempdir().unwrap();
            Sandbox::at(&proc(tmp.path(), info, label))
        };
        let flatpak = at(Some(INFO), None).unwrap();
        assert_eq!(flatpak.app.as_deref(), Some("org.example.player"));
        assert_eq!(
            at(None, Some("snap.demo.app (enforce)")),
            Some(Sandbox::default())
        );
        assert_eq!(at(None, Some("unconfined")), None);
        assert_eq!(at(None, None), None);

        // A process that can't be inspected counts as sandboxed.
        let tmp = tempfile::tempdir().unwrap();
        let gone = tmp.path().join("gone").display().to_string();
        assert_eq!(Sandbox::at(&gone), Some(Sandbox::default()));
    }
}
