//! `client!`: a typed client generated from a schema at compile time.

use std::path::PathBuf;

use proc_macro::{Delimiter, TokenStream, TokenTree};

/// The typed client for the schema at a path relative to the crate root, with only `keys` if listed.
#[proc_macro]
pub fn client(input: TokenStream) -> TokenStream {
    match expand(input) {
        Ok(code) => code,
        Err(message) => format!("compile_error!({message:?});")
            .parse()
            .expect("an escaped string is one token"),
    }
}

fn expand(input: TokenStream) -> Result<TokenStream, String> {
    let root =
        std::env::var("CARGO_MANIFEST_DIR").map_err(|e| format!("CARGO_MANIFEST_DIR: {e}"))?;
    let (relative, keys) = arguments(input)?;
    let path = PathBuf::from(root).join(&relative);
    let at = |e: &dyn std::fmt::Display| format!("{relative}: {e}");
    let source = std::fs::read_to_string(&path).map_err(|e| at(&e))?;
    let mut schema = mxconfig_schema::Schema::parse(&source).map_err(|e| at(&e))?;
    if let Some(keys) = keys {
        let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
        schema = schema.trimmed(&keys).map_err(|e| at(&e))?;
    }
    let code = mxconfig_codegen::rust(&schema).map_err(|e| at(&e))?;
    // Cargo rebuilds a crate when a file it includes changes.
    let tracked = format!(
        "const _: &str = include_str!({:?});",
        path.display().to_string()
    );
    format!("{code}\n{tracked}").parse().map_err(|e| at(&e))
}

/// The usage `client!` accepts.
const USAGE: &str = "expected a string literal and an optional key list, as in `client!(\"schema/org.example.app.toml\", keys = [\"colors.accent\"])`";

/// The path in `client!("...")`, and the keys after `keys =`, if listed.
fn arguments(input: TokenStream) -> Result<(String, Option<Vec<String>>), String> {
    let mut tokens = input.into_iter();
    let path = tokens.next().as_ref().and_then(string).ok_or(USAGE)?;
    let Some(comma) = tokens.next() else {
        return Ok((path, None));
    };
    let (
        Some(TokenTree::Ident(name)),
        Some(TokenTree::Punct(eq)),
        Some(TokenTree::Group(list)),
        None,
    ) = (tokens.next(), tokens.next(), tokens.next(), tokens.next())
    else {
        return Err(USAGE.into());
    };
    let shaped = matches!(&comma, TokenTree::Punct(p) if p.as_char() == ',')
        && name.to_string() == "keys"
        && eq.as_char() == '='
        && list.delimiter() == Delimiter::Bracket;
    if !shaped {
        return Err(USAGE.into());
    }
    let items = list.stream().into_iter();
    let keys = items.filter(|token| !matches!(token, TokenTree::Punct(p) if p.as_char() == ','));
    let keys: Option<Vec<String>> = keys.map(|token| string(&token)).collect();
    Ok((path, Some(keys.ok_or(USAGE)?)))
}

/// The text of a plain string literal.
fn string(token: &TokenTree) -> Option<String> {
    let TokenTree::Literal(literal) = token else {
        return None;
    };
    let text = literal.to_string();
    text.strip_prefix('"')
        .and_then(|text| text.strip_suffix('"'))
        .filter(|text| !text.contains('\\'))
        .map(str::to_string)
}
