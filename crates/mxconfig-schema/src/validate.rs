//! Schema linting and the checks a value must pass for its key.

use crate::error::Diagnostic;
use crate::schema::{KeySchema, KeyType, Schema};
use crate::value::{Value, as_f64, type_name};

impl Schema {
    /// Lint the schema. An empty result means it is valid.
    pub fn validate(&self) -> Vec<Diagnostic> {
        let mut problems = Vec::new();

        // Every step from v1 to the current version needs a migration edge.
        let mut version = 1;
        while version < self.version() {
            match self.edge_from(version) {
                Some(edge) if edge.to > version => version = edge.to,
                Some(edge) => {
                    let message = format!(
                        "migration v{} -> v{} does not move forward",
                        edge.from, edge.to
                    );
                    problems.push(message);
                    break;
                }
                None => {
                    problems.push(format!("no migration from v{version}"));
                    break;
                }
            }
        }

        for group in self.groups().iter().filter(|g| g.label.is_empty()) {
            problems.push(format!("group `{}` has no label", group.id));
        }

        // Declaring an env var that is never read is a mistake.
        let keys = self.keys().iter().filter(|k| k.managed && k.env.is_some());
        let assets = self
            .assets()
            .iter()
            .filter(|a| a.managed && a.env.is_some());
        for owner in keys.map(|k| &k.path).chain(assets.map(|a| &a.id)) {
            problems.push(format!(
                "`{owner}` is managed, so its env var is never read"
            ));
        }

        for key in self.keys() {
            let path = &key.path;
            if matches!(&key.ty, KeyType::Enum(values) if values.is_empty()) {
                problems.push(format!("enum `{path}` has no values"));
            }
            if key.label.is_empty() {
                problems.push(format!("`{path}` has no label"));
            }
            if key.description.is_empty() {
                problems.push(format!("`{path}` has no description"));
            }
            if let Some(default) = &key.default {
                let reasons = key.check(default).into_iter();
                problems.extend(reasons.map(|reason| format!("default for `{path}` {reason}")));
            }
        }

        let lint = |problem| Diagnostic::error(problem).with_source("schema");
        problems.into_iter().map(lint).collect()
    }
}

impl KeySchema {
    /// The reasons `value` is invalid for this key.
    pub fn check(&self, value: &Value) -> Vec<String> {
        if !self.ty.matches(value) {
            let reason = match (&self.ty, value) {
                (KeyType::Enum(values), Value::String(s)) => {
                    format!("is `{s}`, not one of {}", values.join(", "))
                }
                _ => format!(
                    "is {} but the schema declares {}",
                    type_name(value),
                    self.ty
                ),
            };
            return vec![reason];
        }

        let mut reasons = Vec::new();
        if let Some(n) = as_f64(value) {
            if let Some(min) = self.min
                && n < min
            {
                reasons.push(format!("is {n}, below min {min}"));
            }
            if let Some(max) = self.max
                && n > max
            {
                reasons.push(format!("is {n}, above max {max}"));
            }
        }
        if let (Some(max), Value::String(s)) = (self.max_length, value)
            && s.chars().count() as u64 > max
        {
            reasons.push(format!("is longer than max_length {max}"));
        }
        if let KeyType::Array(Some(element)) | KeyType::Map(Some(element)) = &self.ty {
            for (at, item) in mismatches(value, element) {
                let found = type_name(item);
                reasons.push(format!(
                    "has {found} at {at}, but the element type is {element}"
                ));
            }
        }
        reasons
    }
}

/// The `(location, element)` pairs that don't match `element`.
fn mismatches<'a>(value: &'a Value, element: &KeyType) -> Vec<(String, &'a Value)> {
    let bad = |item: &&Value| !element.matches(item);
    match value {
        Value::Array(items) => items
            .iter()
            .enumerate()
            .filter(|(_, item)| bad(item))
            .map(|(i, item)| (format!("[{i}]"), item))
            .collect(),
        Value::Table(map) => map
            .iter()
            .filter(|(_, item)| bad(item))
            .map(|(k, item)| (format!("`{k}`"), item))
            .collect(),
        _ => Vec::new(),
    }
}
