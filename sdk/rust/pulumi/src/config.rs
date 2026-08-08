//! Access to stack configuration.

use std::collections::{HashMap, HashSet};

use crate::error::{Error, Result};
use crate::output::Output;
use crate::value::PropertyValue;

/// Stack configuration: a bag of string values keyed by `project:key`,
/// with a subset marked secret.
#[derive(Clone, Debug, Default)]
pub struct Config {
    values: HashMap<String, String>,
    secret_keys: HashSet<String>,
    project: String,
}

impl Config {
    pub(crate) fn new(
        values: HashMap<String, String>,
        secret_keys: HashSet<String>,
        project: String,
    ) -> Self {
        Config { values, secret_keys, project }
    }

    fn full_key(&self, key: &str) -> String {
        if key.contains(':') {
            key.to_string()
        } else {
            format!("{}:{}", self.project, key)
        }
    }

    /// Get a raw string config value.
    pub fn get(&self, key: &str) -> Option<String> {
        self.values.get(&self.full_key(key)).cloned()
    }

    fn is_secret(&self, key: &str) -> bool {
        self.secret_keys.contains(&self.full_key(key))
    }

    /// Get a config value parsed as JSON when possible, mirroring how Pulumi
    /// stores structured config. Plain strings stay strings.
    pub fn get_value(&self, key: &str) -> Option<PropertyValue> {
        let raw = self.get(key)?;
        let value = parse_config_value(&raw);
        if self.is_secret(key) {
            Some(PropertyValue::Secret(Box::new(value)))
        } else {
            Some(value)
        }
    }

    /// Require a config value, wrapped as an output (secret when the key is
    /// marked secret).
    pub fn require(&self, key: &str) -> Result<Output<PropertyValue>> {
        match self.get_value(key) {
            Some(v) => Ok(Output::from_value(v)),
            None => Err(Error::new(format!("missing required configuration key {key:?}"))),
        }
    }

    /// Like [`Config::require`], but returns `default` when unset.
    pub fn get_or(&self, key: &str, default: PropertyValue) -> Output<PropertyValue> {
        Output::from_value(self.get_value(key).unwrap_or(default))
    }

    fn typed_value(&self, key: &str, parse: fn(&str) -> PropertyValue) -> Option<PropertyValue> {
        let raw = self.get(key)?;
        let value = parse(&raw);
        if self.is_secret(key) {
            Some(PropertyValue::Secret(Box::new(value)))
        } else {
            Some(value)
        }
    }

    fn require_typed(
        &self,
        key: &str,
        parse: fn(&str) -> PropertyValue,
    ) -> Result<Output<PropertyValue>> {
        match self.typed_value(key, parse) {
            Some(v) => Ok(Output::from_value(v)),
            None => Err(Error::new(format!("missing required configuration variable '{key}'"))),
        }
    }

    fn typed_opt(
        &self,
        key: &str,
        parse: fn(&str) -> PropertyValue,
    ) -> Option<Output<PropertyValue>> {
        self.typed_value(key, parse).map(Output::from_value)
    }

    /// Optional typed getters: `Some` when the key is set.
    pub fn get_string_opt(&self, key: &str) -> Option<Output<PropertyValue>> {
        self.typed_opt(key, parse_string)
    }

    pub fn get_number_opt(&self, key: &str) -> Option<Output<PropertyValue>> {
        self.typed_opt(key, parse_number)
    }

    pub fn get_int_opt(&self, key: &str) -> Option<Output<PropertyValue>> {
        self.typed_opt(key, parse_number)
    }

    pub fn get_bool_opt(&self, key: &str) -> Option<Output<PropertyValue>> {
        self.typed_opt(key, parse_bool)
    }

    pub fn get_object_opt(&self, key: &str) -> Option<Output<PropertyValue>> {
        self.typed_opt(key, parse_object)
    }

    fn typed_or(
        &self,
        key: &str,
        parse: fn(&str) -> PropertyValue,
        default: PropertyValue,
    ) -> Output<PropertyValue> {
        Output::from_value(self.typed_value(key, parse).unwrap_or(default))
    }

    /// Require a string-typed config value: the raw value verbatim.
    pub fn require_string(&self, key: &str) -> Result<Output<PropertyValue>> {
        self.require_typed(key, parse_string)
    }

    pub fn get_string_or(&self, key: &str, default: PropertyValue) -> Output<PropertyValue> {
        self.typed_or(key, parse_string, default)
    }

    /// Require a number-typed config value.
    pub fn require_number(&self, key: &str) -> Result<Output<PropertyValue>> {
        self.require_typed(key, parse_number)
    }

    pub fn get_number_or(&self, key: &str, default: PropertyValue) -> Output<PropertyValue> {
        self.typed_or(key, parse_number, default)
    }

    /// Require an int-typed config value.
    pub fn require_int(&self, key: &str) -> Result<Output<PropertyValue>> {
        self.require_typed(key, parse_number)
    }

    pub fn get_int_or(&self, key: &str, default: PropertyValue) -> Output<PropertyValue> {
        self.typed_or(key, parse_number, default)
    }

    /// Require a bool-typed config value.
    pub fn require_bool(&self, key: &str) -> Result<Output<PropertyValue>> {
        self.require_typed(key, parse_bool)
    }

    pub fn get_bool_or(&self, key: &str, default: PropertyValue) -> Output<PropertyValue> {
        self.typed_or(key, parse_bool, default)
    }

    /// Require a structured (JSON) config value.
    pub fn require_object(&self, key: &str) -> Result<Output<PropertyValue>> {
        self.require_typed(key, parse_object)
    }

    pub fn get_object_or(&self, key: &str, default: PropertyValue) -> Output<PropertyValue> {
        self.typed_or(key, parse_object, default)
    }
}

fn parse_string(raw: &str) -> PropertyValue {
    PropertyValue::String(raw.to_string())
}

fn parse_number(raw: &str) -> PropertyValue {
    PropertyValue::Number(raw.parse().unwrap_or(0.0))
}

fn parse_bool(raw: &str) -> PropertyValue {
    PropertyValue::Bool(raw == "true" || raw == "1")
}

fn parse_object(raw: &str) -> PropertyValue {
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(v) => json_to_property(&v),
        Err(_) => PropertyValue::String(raw.to_string()),
    }
}

/// Interpret a raw config string: structured values arrive as JSON, plain
/// strings as themselves.
fn parse_config_value(raw: &str) -> PropertyValue {
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(v) if !v.is_string() => json_to_property(&v),
        _ => PropertyValue::String(raw.to_string()),
    }
}

/// Convert a JSON value to a property value.
pub fn json_to_property(v: &serde_json::Value) -> PropertyValue {
    match v {
        serde_json::Value::Null => PropertyValue::Null,
        serde_json::Value::Bool(b) => PropertyValue::Bool(*b),
        serde_json::Value::Number(n) => PropertyValue::Number(n.as_f64().unwrap_or(0.0)),
        serde_json::Value::String(s) => PropertyValue::String(s.clone()),
        serde_json::Value::Array(a) => {
            PropertyValue::Array(a.iter().map(json_to_property).collect())
        }
        serde_json::Value::Object(o) => PropertyValue::Object(
            o.iter().map(|(k, v)| (k.clone(), json_to_property(v))).collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_values() {
        let mut values = HashMap::new();
        values.insert("proj:aString".to_string(), "hello".to_string());
        values.insert("proj:anInt".to_string(), "42".to_string());
        values.insert("proj:aList".to_string(), "[\"a\",\"b\"]".to_string());
        let mut secrets = HashSet::new();
        secrets.insert("proj:aString".to_string());
        let c = Config::new(values, secrets, "proj".to_string());

        assert_eq!(
            c.get_value("aString"),
            Some(PropertyValue::Secret(Box::new(PropertyValue::String("hello".into()))))
        );
        assert_eq!(c.get_value("anInt"), Some(PropertyValue::Number(42.0)));
        assert_eq!(
            c.get_value("aList"),
            Some(PropertyValue::Array(vec![
                PropertyValue::String("a".into()),
                PropertyValue::String("b".into()),
            ]))
        );
    }
}
