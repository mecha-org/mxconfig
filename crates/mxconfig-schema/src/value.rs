//! The value model: TOML's values, plus the few readers mxconfig needs.

use serde::Deserialize;

use crate::error::{Error, Kind};

pub use toml::Value;

/// The type name used in diagnostics.
pub(crate) fn type_name(value: &Value) -> &'static str {
    match value {
        Value::String(_) => "string",
        Value::Integer(_) => "integer",
        Value::Float(_) => "float",
        Value::Boolean(_) => "bool",
        Value::Datetime(_) => "datetime",
        Value::Array(_) => "array",
        Value::Table(_) => "map",
    }
}

/// The text of a string or datetime value.
pub fn to_text(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Datetime(d) => Some(d.to_string()),
        _ => None,
    }
}

/// A number as `f64`, integers included.
#[expect(clippy::cast_precision_loss, reason = "bounds compare as `f64`")]
pub fn as_f64(value: &Value) -> Option<f64> {
    value
        .as_float()
        .or_else(|| value.as_integer().map(|i| i as f64))
}

/// Parse a bare TOML value, e.g. `35`, `"speakers"`, `[1, 2]`.
pub fn from_toml_str(input: &str) -> Result<Value, Error> {
    let value = toml::de::ValueDeserializer::parse(input).and_then(Value::deserialize);
    value.map_err(|e| Kind::Value(e.to_string()).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lone_value_parses_and_anything_after_it_is_refused() {
        assert_eq!(from_toml_str("35").unwrap(), Value::Integer(35));
        assert_eq!(
            from_toml_str("[1, 2]").unwrap().as_array().unwrap().len(),
            2
        );
        let when = from_toml_str("1979-05-27T07:32:00Z").unwrap();
        assert!(matches!(when, Value::Datetime(_)), "{when:?}");
        assert!(from_toml_str("1\nx = 2").is_err());
        assert!(from_toml_str("").is_err());
    }
}
