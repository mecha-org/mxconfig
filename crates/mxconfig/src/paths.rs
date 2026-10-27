//! File locations and loading.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt::Write;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Diagnostic, Error, Kind};
use crate::sandbox::{FLATPAK_HOST_MOUNT, Sandbox};
use crate::transport::Bus;
use crate::write;
use mxconfig_schema::Class;
use mxconfig_schema::Schema;

/// mxconfig's own directory under each base.
pub(crate) const APP: &str = "mxconfig";

/// The vendor layer's default base, part of the OS image.
const VENDOR_BASE: &str = "/usr/lib";

/// The admin layer's default base.
pub const ADMIN_BASE: &str = "/etc";

/// The default base of installed data, which holds the manifests.
const DATA_BASE: &str = "/usr/share";

/// The directory under mxconfig's own that holds schemas.
pub(crate) const SCHEMAS: &str = "schemas";

/// The directory under mxconfig's own that holds manifests.
const MANIFESTS: &str = "manifests";

/// The extension of schema, manifest and settings files.
const TOML_EXT: &str = "toml";

/// An app namespace's main file.
const APP_MAIN: &str = "config.toml";

/// An app namespace's drop-in directory.
const APP_DROP_INS: &str = "config.d";

/// A shared namespace's main file.
const SHARED_MAIN: &str = "conf.toml";

/// A shared namespace's drop-in directory.
const SHARED_DROP_INS: &str = "conf.d";

/// The env var overriding the vendor base.
const VENDOR_DIR_ENV: &str = "MXCONFIG_VENDOR_DIR";

/// The env var overriding the admin base.
const ADMIN_DIR_ENV: &str = "MXCONFIG_ADMIN_DIR";

/// The env var overriding the user base.
const USER_DIR_ENV: &str = "MXCONFIG_USER_DIR";

/// The env var overriding the schema search path, a `:` list.
const SCHEMA_DIR_ENV: &str = "MXCONFIG_SCHEMA_DIR";

/// The env var overriding the manifest directory.
const MANIFEST_DIR_ENV: &str = "MXCONFIG_MANIFEST_DIR";

/// Suffix of the lock file beside a namespace's user home.
pub(crate) const LOCK_SUFFIX: &str = ".lock";

/// The schema search path and the base directory of each layer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Roots {
    /// The schema search path. The first match wins.
    pub schema_dirs: Vec<PathBuf>,
    /// The vendor layer, part of the OS image: `/usr/lib`.
    pub vendor_dir: Option<PathBuf>,
    /// The admin layer: `/etc`.
    pub admin_dir: Option<PathBuf>,
    /// The user layer, `$XDG_CONFIG_HOME`. `None` without a home directory.
    pub user_dir: Option<PathBuf>,
    /// The bus this source serves clients on, as mxconfigd does.
    pub serving: Option<Bus>,
    /// Installed client manifests, one file per client.
    pub manifest_dir: Option<PathBuf>,
}

impl Roots {
    /// The `MXCONFIG_*_DIR` overrides holding a relative path, which [`Roots::find`] ignores.
    #[doc(hidden)]
    pub fn relative_overrides() -> Vec<&'static str> {
        let keys = [
            SCHEMA_DIR_ENV,
            VENDOR_DIR_ENV,
            ADMIN_DIR_ENV,
            USER_DIR_ENV,
            MANIFEST_DIR_ENV,
        ];
        let relative = |key: &&str| {
            let value = std::env::var_os(key).unwrap_or_default();
            std::env::split_paths(&value).any(|path| path.is_relative() && path != Path::new(""))
        };
        keys.into_iter().filter(relative).collect()
    }

    /// This system's roots: env overrides, else the standard locations.
    pub fn find() -> Roots {
        // A Flatpak app reads only its own directories, as the daemon does.
        if let Some(roots) = Sandbox::current().and_then(|s| s.roots_inside()) {
            return roots;
        }
        Roots {
            schema_dirs: env_dirs(SCHEMA_DIR_ENV).unwrap_or_else(|| {
                data_dirs()
                    .into_iter()
                    .map(|dir| dir.join(APP).join(SCHEMAS))
                    .collect()
            }),
            vendor_dir: Some(env_dir(VENDOR_DIR_ENV).unwrap_or_else(|| VENDOR_BASE.into())),
            admin_dir: Some(env_dir(ADMIN_DIR_ENV).unwrap_or_else(|| ADMIN_BASE.into())),
            user_dir: env_dir(USER_DIR_ENV).or_else(config_home),
            // Manifests control access, so never take them from XDG paths.
            manifest_dir: Some(
                env_dir(MANIFEST_DIR_ENV)
                    .unwrap_or_else(|| Path::new(DATA_BASE).join(APP).join(MANIFESTS)),
            ),
            serving: None,
        }
    }

    /// All roots under one directory. For tests and embedding.
    pub fn under(base: &Path) -> Roots {
        Roots {
            schema_dirs: vec![base.join(SCHEMAS)],
            vendor_dir: Some(base.join("vendor")),
            admin_dir: Some(base.join("admin")),
            user_dir: Some(base.join("user")),
            manifest_dir: Some(base.join(MANIFESTS)),
            serving: None,
        }
    }

    /// `namespace`'s installed schema.
    pub fn schema(&self, namespace: &str) -> Result<Schema, Error> {
        let Some(path) = self.schema_paths(namespace).next() else {
            return Err(Kind::NotInstalled(namespace.to_string()).into());
        };
        let text = fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
        Ok(Schema::parse(&text)?)
    }

    /// The installed schema files for `namespace`, in search order. The first wins.
    pub fn schema_paths(&self, namespace: &str) -> impl Iterator<Item = PathBuf> + '_ {
        let file = format!("{namespace}.{TOML_EXT}");
        let paths = self.schema_dirs.iter().map(move |dir| dir.join(&file));
        paths.filter(|path| path.is_file())
    }

    /// Whether `namespace` has a schema installed.
    pub fn is_installed(&self, namespace: &str) -> bool {
        self.schema_paths(namespace).next().is_some()
    }

    /// All schema files on the search path, the first of each name, sorted.
    pub fn schema_files(&self) -> Vec<PathBuf> {
        let mut files = BTreeMap::new();
        for dir in &self.schema_dirs {
            for name in toml_names(dir) {
                files.entry(name).or_insert_with_key(|name| dir.join(name));
            }
        }
        files.into_values().collect()
    }

    /// The manifest installed for `client`, an app id.
    pub fn manifest_file(&self, client: &str) -> Option<PathBuf> {
        let dir = self.manifest_dir.as_ref();
        dir.map(|dir| dir.join(format!("{client}.{TOML_EXT}")))
    }

    /// All installed manifests, sorted.
    pub fn manifest_files(&self) -> Vec<PathBuf> {
        let Some(dir) = &self.manifest_dir else {
            return Vec::new();
        };
        toml_names(dir)
            .into_iter()
            .map(|name| dir.join(name))
            .collect()
    }

    /// The configured layers, lowest precedence first.
    pub(crate) fn layers(&self) -> Vec<Layer> {
        [
            (LayerKind::Vendor, &self.vendor_dir),
            (LayerKind::Admin, &self.admin_dir),
            (LayerKind::User, &self.user_dir),
        ]
        .into_iter()
        .filter_map(|(kind, root)| {
            Some(Layer {
                kind,
                root: root.clone()?,
            })
        })
        .collect()
    }
}

/// A namespace's directory in one layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Home {
    /// `<base>/<dir>`, or `<base>/mxconfig/<namespace>` when shared.
    pub dir: PathBuf,
    /// The highest directory that may be watched. Never `<base>` itself.
    pub bound: PathBuf,
    shared: bool,
}

impl Home {
    /// `schema`'s home under `base`.
    pub fn of(base: &Path, schema: &Schema) -> Home {
        if schema.class() == Class::App {
            let dir = base.join(schema.dir());
            return Home {
                bound: dir.clone(),
                dir,
                shared: false,
            };
        }
        let bound = base.join(APP);
        Home {
            dir: bound.join(schema.namespace()),
            bound,
            shared: true,
        }
    }

    /// The main file: `config.toml`, or `conf.toml` when shared.
    pub fn main_file(&self) -> PathBuf {
        self.dir
            .join(if self.shared { SHARED_MAIN } else { APP_MAIN })
    }

    /// The drop-in directory: `config.d`, or `conf.d` when shared.
    pub fn drop_in_dir(&self) -> PathBuf {
        let name = if self.shared {
            SHARED_DROP_INS
        } else {
            APP_DROP_INS
        };
        self.dir.join(name)
    }
}

/// Files in the order they apply: system files, system drop-ins, then user.
pub(crate) fn sources(layers: &[Layer], schema: &Schema) -> Vec<(LayerKind, PathBuf)> {
    let (user, system): (Vec<&Layer>, _) = layers.iter().partition(|l| l.kind == LayerKind::User);
    let mut files: Vec<(LayerKind, PathBuf)> = system
        .iter()
        .map(|layer| (layer.kind, layer.home(schema).main_file()))
        .collect();

    let mut drop_ins = BTreeMap::new();
    for layer in &system {
        let dir = layer.home(schema).drop_in_dir();
        // A higher layer's drop-in replaces a lower one with the same name.
        drop_ins.extend(toml_names(&dir).into_iter().map(|name| {
            let path = dir.join(&name);
            (name, (layer.kind, path))
        }));
    }
    files.extend(drop_ins.into_values());

    for layer in user {
        let home = layer.home(schema);
        files.push((layer.kind, home.main_file()));
        let dir = home.drop_in_dir();
        let names = toml_names(&dir).into_iter();
        files.extend(names.map(|name| (layer.kind, dir.join(name))));
    }

    // Skip missing and masked files.
    files.retain(|(_, path)| path.is_file());
    files
}

/// Whether a file name ends in `.toml`.
pub(crate) fn is_toml(name: &OsStr) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|ext| ext == TOML_EXT)
}

/// Sorted `*.toml` names in `dir`, including `/dev/null` links.
fn toml_names(dir: &Path) -> Vec<OsString> {
    let mut names: Vec<_> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name())
                .filter(|name| is_toml(name))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// Whether the current user can write `path`.
pub(crate) fn user_writable(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    meta.mode() & 0o002 != 0 || meta.uid() == nix::unistd::geteuid().as_raw()
}

/// Create `dir` and missing parents with mode 0700, as XDG requires.
pub(crate) fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

/// `$XDG_CONFIG_HOME`, else `~/.config`.
fn config_home() -> Option<PathBuf> {
    env_dir("XDG_CONFIG_HOME").or_else(|| Some(home_dir()?.join(".config")))
}

/// `$XDG_DATA_HOME`, then `$XDG_DATA_DIRS`, with XDG's defaults.
fn data_dirs() -> Vec<PathBuf> {
    let home = env_dir("XDG_DATA_HOME").or_else(|| Some(home_dir()?.join(".local/share")));
    let system = env_dirs("XDG_DATA_DIRS")
        .unwrap_or_else(|| vec!["/usr/local/share".into(), "/usr/share".into()]);
    // A sandbox sees host dirs under /run/host, but their schemas aren't its own.
    let own = |dir: &PathBuf| !dir.starts_with(FLATPAK_HOST_MOUNT);
    home.into_iter().chain(system).filter(own).collect()
}

/// `$HOME`, else the user's home from the password database.
pub(crate) fn home_dir() -> Option<PathBuf> {
    env_dir("HOME").or_else(|| std::env::home_dir().filter(|dir| dir.is_absolute()))
}

/// An absolute path from env var `key`. XDG ignores empty and relative ones.
fn env_dir(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

/// Absolute paths from the `:`-separated env var `key`, if any.
fn env_dirs(key: &str) -> Option<Vec<PathBuf>> {
    let value = std::env::var_os(key)?;
    let dirs: Vec<PathBuf> = std::env::split_paths(&value)
        .filter(|path| path.is_absolute())
        .collect();
    (!dirs.is_empty()).then_some(dirs)
}

/// An env var's value. Empty or non-UTF-8 counts as unset.
pub(crate) fn env_var(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// The layer a file belongs to, lowest precedence first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum LayerKind {
    /// Vendor defaults, part of the OS image.
    Vendor,
    /// System administrator settings.
    Admin,
    /// The user's settings.
    User,
}

impl std::fmt::Display for LayerKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            LayerKind::Vendor => "vendor",
            LayerKind::Admin => "admin",
            LayerKind::User => "user",
        })
    }
}

/// One layer's base directory.
#[derive(Debug, Clone)]
pub(crate) struct Layer {
    pub(crate) kind: LayerKind,
    pub(crate) root: PathBuf,
}

impl Layer {
    /// `schema`'s directory in this layer.
    pub(crate) fn home(&self, schema: &Schema) -> Home {
        Home::of(&self.root, schema)
    }
}

/// A parse error as one line, with its line number.
fn toml_error(text: &str, e: &toml::de::Error) -> String {
    let message = e.message().trim().replace('\n', ": ");
    match e.span() {
        Some(span) => {
            let line = text[..span.start.min(text.len())].matches('\n').count() + 1;
            format!("line {line}: {message}")
        }
        None => message,
    }
}

/// Read a TOML file, falling back to its last good copy if it is broken.
pub(crate) fn read_doc(
    path: &Path,
    user: bool,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<toml::Value> {
    if !path.is_file() {
        return None;
    }
    // Only problems name the file, so build its name only for them.
    let source = || path.display().to_string();
    let text = fs::read_to_string(path)
        .inspect_err(|e| {
            let message = format!("cannot read file: {e}");
            diagnostics.push(Diagnostic::error(message).with_source(source()));
        })
        .ok()?;
    let e = match toml::from_str::<toml::Value>(&text) {
        Ok(doc) => {
            if user && let Err(e) = write::snapshot(path, &text) {
                diagnostics.push(
                    Diagnostic::warning(format!("no last good copy: {e}")).with_source(source()),
                );
            }
            return Some(doc);
        }
        Err(e) => toml_error(&text, &e),
    };
    let fallback = fs::read_to_string(write::lastgood_path(path))
        .ok()
        .and_then(|text| toml::from_str::<toml::Value>(&text).ok());
    let mut message = match fallback {
        Some(_) => format!("invalid TOML, so its last good copy is read: {e}"),
        None => format!("invalid TOML: {e}"),
    };
    // Copy, don't move: the user may still be editing it.
    if user {
        let _ = match write::keep_aside(path, &text) {
            Ok(aside) => write!(message, "; kept as {}", aside.display()),
            Err(e) => write!(message, "; not kept aside: {e}"),
        };
    }
    diagnostics.push(Diagnostic::error(message).with_source(source()));
    fallback
}
