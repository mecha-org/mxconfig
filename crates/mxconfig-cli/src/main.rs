//! Validate schemas, and inspect or change settings in their files.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use mxconfig::host::{Roots, Sandbox};
use mxconfig::{Diagnostic, Edit, KeySchema, KeyType, Manifest, Origin, Schema};
use mxconfig::{Settings, Source, Value, value};

#[derive(Parser)]
#[command(
    name = "mxconfig",
    about = "Validate schemas, and inspect or change mxconfig settings."
)]
struct Cli {
    /// Use a Flatpak app's own settings, even if the host has a schema too.
    #[arg(long, global = true)]
    flatpak: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Validate schemas and manifests, or every installed one.
    Check {
        /// Schema or manifest files to validate.
        files: Vec<PathBuf>,
    },
    /// Print a namespace's effective values and where each came from.
    Dump {
        /// Namespace id, e.g. `org.mechanix.demo`.
        namespace: String,
    },
    /// Print a namespace's groups and keys, as a settings UI reads them.
    Describe {
        /// Namespace id, e.g. `org.mechanix.demo`.
        namespace: String,
    },
    /// Print a namespace's declared assets and where each comes from.
    Assets {
        /// Namespace id, e.g. `org.mechanix.demo`.
        namespace: String,
    },
    /// Drop keys the schema does not define, and retired asset files, from the user's copy.
    Prune {
        /// Namespace id, e.g. `org.mechanix.demo`.
        namespace: String,
    },
    /// Set a key in the user's file. Running apps follow the change.
    Set {
        /// Namespace id, e.g. `org.mechanix.desktop.display`.
        namespace: String,
        /// Key path, e.g. `brightness`.
        key: String,
        /// A TOML value, e.g. `60`, `true` or `[1, 2]`. A string key takes plain text.
        value: String,
    },
    /// Unset a key in the user's file, so a lower layer or the default applies.
    Unset {
        /// Namespace id, e.g. `org.mechanix.desktop.display`.
        namespace: String,
        /// Key path, e.g. `brightness`.
        key: String,
    },
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    // A relative override would be ignored, and the real files used instead.
    if let Some(key) = Roots::relative_overrides().first() {
        bail!("{key} must be an absolute path");
    }
    let load = |namespace: &str| open(namespace, cli.flatpak);
    match cli.command {
        Command::Check { files } => check(files)?,
        Command::Dump { namespace } => dump(&load(&namespace)?),
        Command::Describe { namespace } => describe(&load(&namespace)?),
        Command::Assets { namespace } => assets(&load(&namespace)?),
        Command::Prune { namespace } => prune(&load(&namespace)?)?,
        Command::Set {
            namespace,
            key,
            value,
        } => {
            let settings = load(&namespace)?;
            let value = value_of(&settings, &key, &value)?;
            write(&settings, Edit::set(key, value))?;
        }
        Command::Unset { namespace, key } => write(&load(&namespace)?, Edit::remove(key))?,
    }
    Ok(())
}

/// `text` as a value for `key`. A string key takes plain text, and a float key whole numbers.
fn value_of(settings: &Settings, key: &str, text: &str) -> Result<Value> {
    let schema = settings.describe();
    let parsed = value::from_toml_str(text);
    Ok(match (schema.key(key).map(|key| &key.ty), parsed) {
        (Some(KeyType::String | KeyType::Enum(_)), Ok(Value::String(text))) => Value::String(text),
        (Some(KeyType::String | KeyType::Enum(_)), _) => Value::String(text.into()),
        (Some(KeyType::Float), _) if let Ok(float) = text.parse() => Value::Float(float),
        // The parser's own message only repeats the text.
        (_, parsed) => parsed.map_err(|_parse| {
            anyhow!("`{text}` is not a TOML value; quote a string as '\"{text}\"'")
        })?,
    })
}

/// Write `edit` to the user's file, as the app itself would.
fn write(settings: &Settings, edit: Edit) -> Result<()> {
    let ticket = settings.write(&[edit])?;
    let outcomes = settings.source().outcomes();
    let written = outcomes.iter().find_map(|outcome| ticket.written(outcome));
    Ok(written.expect("a file write finishes at once")?)
}

/// Open `namespace` from the host's files, else from its Flatpak app's.
fn open(namespace: &str, flatpak: bool) -> Result<Settings> {
    let roots = if flatpak {
        let app = Sandbox::installed(namespace).and_then(|app| app.roots());
        app.with_context(|| format!("`{namespace}` is not an installed Flatpak app"))?
    } else {
        let host = Roots::find();
        let app = Sandbox::instead_of(&host, namespace).and_then(|app| app.roots());
        app.unwrap_or(host)
    };
    // A tool uses the files themselves, shared namespaces included.
    let schema = roots.schema(namespace)?;
    Ok(Source::with_roots(roots).open_schema(schema)?)
}

fn check(files: Vec<PathBuf>) -> Result<()> {
    let roots = Roots::find();
    let mut targets = if files.is_empty() {
        let schemas = roots.schema_files().into_iter();
        schemas.chain(roots.manifest_files()).collect()
    } else {
        files
    };
    targets.sort();
    if targets.is_empty() {
        let dirs: Vec<String> = roots
            .schema_dirs
            .iter()
            .map(|d| d.display().to_string())
            .collect();
        println!("no schemas found in {}", dirs.join(":"));
        return Ok(());
    }

    let mut failed = 0;
    for path in targets {
        let source = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        // A manifest has a `client` field. A schema has a `namespace`.
        let is_manifest = source
            .parse::<mxconfig::Table>()
            .is_ok_and(|t| t.contains_key("client"));
        let ok = if is_manifest {
            check_manifest(&path, &source)
        } else {
            check_schema(&roots, &path, &source)
        };
        if !ok {
            failed += 1;
        }
    }

    if failed > 0 {
        bail!("{failed} file(s) failed");
    }
    Ok(())
}

/// Check one schema. Returns whether it is valid.
fn check_schema(roots: &Roots, path: &Path, source: &str) -> bool {
    let schema = match Schema::parse(source) {
        Ok(schema) => schema,
        Err(error) => {
            println!("invalid {}: {error}", path.display());
            return false;
        }
    };
    let diags = schema.validate();
    let ok = !diags.iter().any(Diagnostic::is_error);
    if ok {
        println!(
            "ok      {} ({} keys, {} groups, v{})",
            schema.namespace(),
            schema.keys().len(),
            schema.groups().len(),
            schema.version()
        );
    } else {
        println!("invalid {} ({})", schema.namespace(), path.display());
    }
    print_diagnostics(&diags);
    let found: Vec<_> = roots.schema_paths(schema.namespace()).collect();
    if found.len() > 1 {
        println!(
            "  warning: installed {} times; {} wins",
            found.len(),
            found[0].display()
        );
    }
    ok
}

/// Check one manifest. Returns whether it is valid.
fn check_manifest(path: &Path, source: &str) -> bool {
    let manifest = match Manifest::parse(source) {
        Ok(manifest) => manifest,
        Err(error) => {
            println!("invalid {}: {error}", path.display());
            return false;
        }
    };
    let diags = manifest.validate();
    let status = if diags.is_empty() {
        "ok     "
    } else {
        "invalid"
    };
    println!("{status} {} manifest ({})", manifest.client, path.display());
    print_diagnostics(&diags);
    diags.is_empty()
}

fn print_diagnostics(diags: &[Diagnostic]) {
    for diagnostic in diags {
        println!("  {diagnostic}");
    }
}

/// Print a schema's one-line summary.
fn print_header(schema: &Schema) {
    println!(
        "{} — {} class, schema v{}",
        schema.namespace(),
        schema.class(),
        schema.version()
    );
}

fn dump(settings: &Settings) {
    let resolved = settings.resolved();
    print_header(&settings.describe());
    println!("values:");
    for (key, value) in resolved.iter() {
        let origin = resolved.origin(key).expect("every value has an origin");
        println!("  {key} = {value}    [{origin}]");
    }

    let diagnostics = resolved.diagnostics();
    if !diagnostics.is_empty() {
        println!("problems:");
    }
    print_diagnostics(diagnostics);
}

fn describe(settings: &Settings) {
    let schema = settings.describe();
    print_header(&schema);
    for group in schema.groups() {
        println!("\n[{}] {}", group.id, group.label);
        schema.in_group(&group.id).for_each(print_key);
    }
    if schema.in_group("").next().is_some() {
        println!("\n[ungrouped]");
        schema.in_group("").for_each(print_key);
    }
}

fn print_key(key: &KeySchema) {
    let mut bounds = String::new();
    if let Some(min) = key.min {
        bounds.push_str(&format!(", min {min}"));
    }
    if let Some(max) = key.max {
        bounds.push_str(&format!(", max {max}"));
    }
    if let Some(max) = key.max_length {
        bounds.push_str(&format!(", max_length {max}"));
    }
    if let KeyType::Enum(values) = &key.ty {
        bounds.push_str(&format!(", one of [{}]", values.join(", ")));
    }
    if key.managed {
        bounds.push_str(", managed");
    }
    let default = key
        .default
        .as_ref()
        .map_or("unset".into(), ToString::to_string);
    let env = key.env.as_ref().map(|var| format!(", env ${var}"));
    let env = env.unwrap_or_default();
    println!("  {} — {}", key.path, key.label);
    println!("    type {}{bounds}, default {default}{env}", key.ty);
    println!("    {}", key.description);
}

/// Print a namespace's declared assets and where each comes from.
fn assets(settings: &Settings) {
    let schema = settings.describe();
    if schema.assets().is_empty() {
        println!("no assets");
        return;
    }
    for asset in schema.assets() {
        match settings.asset(&asset.id) {
            Some(found) => {
                let size = std::fs::metadata(&found.path).map_or(0, |meta| meta.len());
                let shown = match found.origin {
                    Origin::Env { .. } => found.path.display().to_string(),
                    _ => asset.path.clone(),
                };
                let (id, origin) = (&asset.id, &found.origin);
                println!("{id} = {shown} ({size} bytes)    [{origin}]");
            }
            None => println!("{} = {}    [unset]", asset.id, asset.path),
        }
    }
}

fn prune(settings: &Settings) -> Result<()> {
    let removed = settings.prune()?;
    if removed.is_empty() {
        println!("nothing to prune");
    }
    for key in removed {
        println!("removed {key}");
    }
    Ok(())
}
