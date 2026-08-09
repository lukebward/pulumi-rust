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

    fn config(pairs: &[(&str, &str)], secrets: &[&str]) -> Config {
        Config::new(
            pairs.iter().map(|(k, v)| (format!("proj:{k}"), v.to_string())).collect(),
            secrets.iter().map(|k| format!("proj:{k}")).collect(),
            "proj".to_string(),
        )
    }

    #[test]
    fn a_bare_key_is_scoped_to_the_project_but_a_qualified_one_is_not() {
        let c = Config::new(
            [
                ("proj:mine".to_string(), "a".to_string()),
                ("aws:region".to_string(), "us-west-2".to_string()),
            ]
            .into_iter()
            .collect(),
            HashSet::new(),
            "proj".to_string(),
        );
        assert_eq!(c.get("mine"), Some("a".to_string()));
        assert_eq!(c.get("aws:region"), Some("us-west-2".to_string()));
        // A bare key never reaches another package's namespace.
        assert_eq!(c.get("region"), None);
    }

    #[tokio::test]
    async fn a_secret_key_produces_a_secret_output_through_every_accessor() {
        // Secretness has to survive the typed accessors, not just get_value —
        // this is what keeps a password from landing in plaintext state.
        let c = config(&[("pw", "hunter2"), ("n", "1"), ("b", "true")], &["pw", "n", "b"]);
        assert!(c.require_string("pw").unwrap().data().await.secret);
        assert!(c.get_string_opt("pw").unwrap().data().await.secret);
        assert!(c.require_number("n").unwrap().data().await.secret);
        assert!(c.get_bool_or("b", PropertyValue::Bool(false)).data().await.secret);
    }

    #[test]
    fn requiring_an_absent_key_names_it_in_the_error() {
        let c = config(&[], &[]);
        let err = c.require_string("missing").unwrap_err().to_string();
        assert!(err.contains("missing"), "unhelpful error: {err}");
    }

    #[tokio::test]
    async fn typed_getters_fall_back_to_the_default_only_when_unset() {
        let c = config(&[("set", "9")], &[]);
        let got = c.get_int_or("set", PropertyValue::Number(0.0)).data().await;
        assert_eq!(got.value, PropertyValue::Number(9.0));
        let dflt = c.get_int_or("unset", PropertyValue::Number(7.0)).data().await;
        assert_eq!(dflt.value, PropertyValue::Number(7.0));
    }

    #[test]
    fn a_string_getter_does_not_parse_its_value_as_json() {
        // "42" and "true" are legal JSON, but asking for a string must give
        // back the text, or a config value like a version number changes type.
        let c = config(&[("v", "42"), ("t", "true")], &[]);
        assert_eq!(c.typed_value("v", parse_string), Some(PropertyValue::String("42".into())));
        assert_eq!(c.typed_value("t", parse_string), Some(PropertyValue::String("true".into())));
    }

    #[test]
    fn bool_parsing_accepts_the_two_forms_pulumi_writes() {
        assert_eq!(parse_bool("true"), PropertyValue::Bool(true));
        assert_eq!(parse_bool("1"), PropertyValue::Bool(true));
        assert_eq!(parse_bool("false"), PropertyValue::Bool(false));
        assert_eq!(parse_bool("anything else"), PropertyValue::Bool(false));
    }

    #[test]
    fn an_unparseable_object_falls_back_to_the_raw_string() {
        assert_eq!(parse_object("{not json"), PropertyValue::String("{not json".into()));
    }

    #[test]
    fn a_plain_string_config_value_is_not_json_decoded() {
        // parse_config_value only decodes non-string JSON, so a value that
        // happens to be quoted text stays text.
        assert_eq!(parse_config_value("hello"), PropertyValue::String("hello".into()));
        assert_eq!(parse_config_value("[1]"), PropertyValue::Array(vec![PropertyValue::Number(1.0)]));
    }

    #[test]
    fn json_to_property_maps_every_json_shape() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"n":1,"s":"x","b":true,"nil":null,"a":[1,2],"o":{"k":"v"}}"#,
        )
        .unwrap();
        match json_to_property(&v) {
            PropertyValue::Object(m) => {
                assert_eq!(m.get("n"), Some(&PropertyValue::Number(1.0)));
                assert_eq!(m.get("s"), Some(&PropertyValue::String("x".into())));
                assert_eq!(m.get("b"), Some(&PropertyValue::Bool(true)));
                assert_eq!(m.get("nil"), Some(&PropertyValue::Null));
                assert!(matches!(m.get("a"), Some(PropertyValue::Array(_))));
                assert!(matches!(m.get("o"), Some(PropertyValue::Object(_))));
            }
            other => panic!("expected an object, got {other:?}"),
        }
    }
}
