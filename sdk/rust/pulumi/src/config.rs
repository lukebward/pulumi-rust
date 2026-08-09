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

    /// A config value that does not parse as the type the program asked for.
    ///
    /// The value is quoted into the message the way the other Pulumi SDKs
    /// report a config type error — except when the key is secret, where
    /// printing it would publish the secret: this text reaches the engine
    /// through `log_error` and is written to the update log in plaintext.
    fn type_error(&self, key: &str, expected: &str, raw: &str) -> Error {
        let full = self.full_key(key);
        if self.is_secret(key) {
            return Error::new(format!(
                "configuration key '{full}' is not a valid {expected} \
                 (the value is secret, so it is not shown)"
            ));
        }
        Error::new(format!(
            "configuration key '{full}' value '{raw}' is not a valid {expected}"
        ))
    }

    fn typed_value(
        &self,
        key: &str,
        expected: &str,
        parse: fn(&str) -> Option<PropertyValue>,
    ) -> Result<Option<PropertyValue>> {
        let raw = match self.get(key) {
            Some(raw) => raw,
            None => return Ok(None),
        };
        // A value that does not parse is a mistake in the stack's config, not
        // a zero. Coercing it silently provisioned zero replicas from
        // `replicas: abc`, and read `enabled: TRUE` as false.
        let value = match parse(&raw) {
            Some(v) => v,
            None => return Err(self.type_error(key, expected, &raw)),
        };
        if self.is_secret(key) {
            Ok(Some(PropertyValue::Secret(Box::new(value))))
        } else {
            Ok(Some(value))
        }
    }

    fn require_typed(
        &self,
        key: &str,
        expected: &str,
        parse: fn(&str) -> Option<PropertyValue>,
    ) -> Result<Output<PropertyValue>> {
        match self.typed_value(key, expected, parse)? {
            Some(v) => Ok(Output::from_value(v)),
            None => Err(Error::new(format!("missing required configuration variable '{key}'"))),
        }
    }

    fn typed_opt(
        &self,
        key: &str,
        expected: &str,
        parse: fn(&str) -> Option<PropertyValue>,
    ) -> Option<Output<PropertyValue>> {
        or_abort(self.typed_value(key, expected, parse)).map(Output::from_value)
    }

    /// Optional typed getters: `Some` when the key is set.
    pub fn get_string_opt(&self, key: &str) -> Option<Output<PropertyValue>> {
        self.typed_opt(key, "string", parse_string)
    }

    pub fn get_number_opt(&self, key: &str) -> Option<Output<PropertyValue>> {
        self.typed_opt(key, "number", parse_number)
    }

    pub fn get_int_opt(&self, key: &str) -> Option<Output<PropertyValue>> {
        self.typed_opt(key, "int", parse_number)
    }

    pub fn get_bool_opt(&self, key: &str) -> Option<Output<PropertyValue>> {
        self.typed_opt(key, "bool", parse_bool)
    }

    pub fn get_object_opt(&self, key: &str) -> Option<Output<PropertyValue>> {
        self.typed_opt(key, "object", parse_object)
    }

    fn typed_or(
        &self,
        key: &str,
        expected: &str,
        parse: fn(&str) -> Option<PropertyValue>,
        default: PropertyValue,
    ) -> Output<PropertyValue> {
        let value = or_abort(self.typed_value(key, expected, parse));
        Output::from_value(value.unwrap_or(default))
    }

    /// Require a string-typed config value: the raw value verbatim.
    pub fn require_string(&self, key: &str) -> Result<Output<PropertyValue>> {
        self.require_typed(key, "string", parse_string)
    }

    pub fn get_string_or(&self, key: &str, default: PropertyValue) -> Output<PropertyValue> {
        self.typed_or(key, "string", parse_string, default)
    }

    /// Require a number-typed config value.
    pub fn require_number(&self, key: &str) -> Result<Output<PropertyValue>> {
        self.require_typed(key, "number", parse_number)
    }

    pub fn get_number_or(&self, key: &str, default: PropertyValue) -> Output<PropertyValue> {
        self.typed_or(key, "number", parse_number, default)
    }

    /// Require an int-typed config value.
    pub fn require_int(&self, key: &str) -> Result<Output<PropertyValue>> {
        self.require_typed(key, "int", parse_number)
    }

    pub fn get_int_or(&self, key: &str, default: PropertyValue) -> Output<PropertyValue> {
        self.typed_or(key, "int", parse_number, default)
    }

    /// Require a bool-typed config value.
    pub fn require_bool(&self, key: &str) -> Result<Output<PropertyValue>> {
        self.require_typed(key, "bool", parse_bool)
    }

    pub fn get_bool_or(&self, key: &str, default: PropertyValue) -> Output<PropertyValue> {
        self.typed_or(key, "bool", parse_bool, default)
    }

    /// Require a structured (JSON) config value.
    pub fn require_object(&self, key: &str) -> Result<Output<PropertyValue>> {
        self.require_typed(key, "object", parse_object)
    }

    pub fn get_object_or(&self, key: &str, default: PropertyValue) -> Output<PropertyValue> {
        self.typed_or(key, "object", parse_object, default)
    }
}

/// Resolve a typed value where the caller has no error channel.
///
/// `get_*_or` and `get_*_opt` are emitted by the code generator into
/// expression position — `let replicas = ctx.config().get_int_or("r", d);` —
/// so making them return a `Result` would change the signature of every
/// typed getter and every call site that uses one. A config value of the
/// wrong type is a mistake in the stack's configuration that no program can
/// carry on from, so it stops the program here with the same message
/// `require_*` returns. The Go SDK does the same (`contract.Failf` in
/// `config.GetInt`), and this crate already stops a program this way for
/// `pv::single_or_none` and for a failed output conversion.
fn or_abort(v: Result<Option<PropertyValue>>) -> Option<PropertyValue> {
    match v {
        Ok(v) => v,
        Err(e) => panic!("{e}"),
    }
}

/// A parser returns `None` for a value that is not of the asked-for type.
fn parse_string(raw: &str) -> Option<PropertyValue> {
    Some(PropertyValue::String(raw.to_string()))
}

fn parse_number(raw: &str) -> Option<PropertyValue> {
    raw.parse().ok().map(PropertyValue::Number)
}

/// Parse a bool the way Pulumi writes one.
///
/// Anything else is rejected rather than read as false: `enabled: TRUE`
/// silently disabling a feature is the kind of bug that only shows up in
/// production. The other Pulumi SDKs raise a config type error here too.
fn parse_bool(raw: &str) -> Option<PropertyValue> {
    match raw {
        "true" | "1" => Some(PropertyValue::Bool(true)),
        "false" | "0" => Some(PropertyValue::Bool(false)),
        _ => None,
    }
}

/// Structured config is JSON; anything that is not JSON is the string
/// itself, so this parser never rejects a value.
fn parse_object(raw: &str) -> Option<PropertyValue> {
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(v) => Some(json_to_property(&v)),
        Err(_) => Some(PropertyValue::String(raw.to_string())),
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
        assert_eq!(
            c.typed_value("v", "string", parse_string).unwrap(),
            Some(PropertyValue::String("42".into()))
        );
        assert_eq!(
            c.typed_value("t", "string", parse_string).unwrap(),
            Some(PropertyValue::String("true".into()))
        );
    }

    #[test]
    fn bool_parsing_accepts_the_forms_pulumi_writes_and_rejects_the_rest() {
        // This used to read every other string as false, so `enabled: TRUE`
        // quietly turned a feature off. Both spellings of each value are
        // still accepted; anything else is a config type error, which is
        // what the other Pulumi SDKs raise.
        assert_eq!(parse_bool("true"), Some(PropertyValue::Bool(true)));
        assert_eq!(parse_bool("1"), Some(PropertyValue::Bool(true)));
        assert_eq!(parse_bool("false"), Some(PropertyValue::Bool(false)));
        assert_eq!(parse_bool("0"), Some(PropertyValue::Bool(false)));
        assert_eq!(parse_bool("TRUE"), None);
        assert_eq!(parse_bool("anything else"), None);
    }

    #[test]
    fn an_unparseable_object_falls_back_to_the_raw_string() {
        assert_eq!(parse_object("{not json"), Some(PropertyValue::String("{not json".into())));
    }

    #[test]
    fn a_malformed_number_is_a_config_error_not_a_zero() {
        // `require_int` returning 0 for `replicas: abc` provisioned zero
        // replicas and reported nothing.
        let c = config(&[("replicas", "abc")], &[]);
        let err = c.require_int("replicas").unwrap_err().to_string();
        assert!(err.contains("proj:replicas"), "error does not name the key: {err}");
        assert!(err.contains("int"), "error does not name the type: {err}");
        assert!(err.contains("abc"), "error does not show the value: {err}");
    }

    #[test]
    fn a_malformed_bool_is_a_config_error_not_a_false() {
        let c = config(&[("enabled", "TRUE")], &[]);
        let err = c.require_bool("enabled").unwrap_err().to_string();
        assert!(err.contains("proj:enabled"), "error does not name the key: {err}");
        assert!(err.contains("bool"), "error does not name the type: {err}");
    }

    #[test]
    fn a_config_type_error_never_prints_a_secret_value() {
        // `runtime.rs` hands this text to the engine with `log_error`, which
        // writes it to the update log in plaintext.
        let c = config(&[("n", "hunter2")], &["n"]);
        let err = c.require_number("n").unwrap_err().to_string();
        assert!(!err.contains("hunter2"), "the secret leaked into the error: {err}");
        assert!(err.contains("proj:n"), "error does not name the key: {err}");
        assert!(err.contains("number"), "error does not name the type: {err}");
    }

    #[test]
    #[should_panic(expected = "proj:replicas")]
    fn a_malformed_number_stops_a_getter_that_has_no_error_channel() {
        // `get_*_or` is emitted into expression position, so it cannot
        // return the error; falling back to the default would hide a
        // misconfigured stack behind a value the program never asked for.
        let c = config(&[("replicas", "abc")], &[]);
        let _ = c.get_int_or("replicas", PropertyValue::Number(1.0));
    }

    #[test]
    #[should_panic(expected = "proj:enabled")]
    fn a_malformed_bool_stops_an_optional_getter_too() {
        let c = config(&[("enabled", "yes")], &[]);
        let _ = c.get_bool_opt("enabled");
    }

    #[tokio::test]
    async fn a_well_formed_value_is_unaffected_by_the_type_check() {
        let c = config(&[("n", "42"), ("b", "false"), ("s", "abc")], &[]);
        assert_eq!(
            c.get_int_or("n", PropertyValue::Number(0.0)).data().await.value,
            PropertyValue::Number(42.0)
        );
        assert_eq!(
            c.get_bool_or("b", PropertyValue::Bool(true)).data().await.value,
            PropertyValue::Bool(false)
        );
        // A string getter takes the value verbatim, so it can never fail.
        assert_eq!(
            c.get_string_or("s", PropertyValue::Null).data().await.value,
            PropertyValue::String("abc".into())
        );
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
