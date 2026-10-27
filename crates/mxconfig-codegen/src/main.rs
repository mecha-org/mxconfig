//! Generate typed clients: <rust|dart|all> <schema.toml> [out-dir] [--keys a,b].

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use mxconfig_schema::Schema;

const USAGE: &str = "usage: mxconfig-codegen <rust|dart|all> <schema.toml> [out-dir] [--keys a,b]";

/// Generates the files for one mode, as `(file_name, contents)` pairs.
type Generate = fn(&Schema) -> Result<Vec<(String, String)>, String>;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let generate: Generate = match args.next().as_deref() {
        Some("rust") => |schema| Ok(vec![mxconfig_codegen::rust_file(schema)?]),
        Some("dart") => |schema| Ok(vec![mxconfig_codegen::dart_file(schema)?]),
        Some("all") => mxconfig_codegen::all,
        _ => bail!(USAGE),
    };
    let schema_path = args.next().context(USAGE)?;
    let rest: Vec<String> = args.collect();
    let (out_dir, keys) = match rest.as_slice() {
        [] => (None, None),
        [dir] => (Some(dir), None),
        [flag, keys] if flag == "--keys" => (None, Some(keys)),
        [dir, flag, keys] if flag == "--keys" => (Some(dir), Some(keys)),
        _ => bail!(USAGE),
    };

    let source =
        fs::read_to_string(&schema_path).with_context(|| format!("cannot read {schema_path}"))?;
    let mut schema =
        Schema::parse(&source).with_context(|| format!("cannot parse {schema_path}"))?;
    if let Some(keys) = keys {
        let keys: Vec<&str> = keys.split(',').map(str::trim).collect();
        schema = schema
            .trimmed(&keys)
            .context("cannot keep only those keys")?;
    }
    for problem in schema.validate() {
        eprintln!("{problem}");
    }

    let files = generate(&schema).map_err(anyhow::Error::msg)?;
    if let Some(dir) = out_dir {
        fs::create_dir_all(dir).with_context(|| format!("cannot create {dir}"))?;
    }
    for (name, body) in files {
        match out_dir {
            Some(dir) => {
                let path = Path::new(dir).join(&name);
                fs::write(&path, &body)
                    .with_context(|| format!("cannot write {}", path.display()))?;
                eprintln!("wrote {}", path.display());
            }
            None => print!("{body}"),
        }
    }
    Ok(())
}
