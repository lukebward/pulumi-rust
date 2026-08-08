//! Dynamic-value helpers used by generated Pulumi Rust programs.
//!
//! Generated programs evaluate PCL expressions in "dynamic" space: every
//! expression is an `Output<PropertyValue>`. These constructors keep the
//! generated code compact.

use crate::output::{all, Output, OutputData};
use crate::value::PropertyValue;

/// A known string output.
pub fn string(s: impl Into<String>) -> Output<PropertyValue> {
    Output::from_value(PropertyValue::String(s.into()))
}

/// A known number output.
pub fn number(n: f64) -> Output<PropertyValue> {
    Output::from_value(PropertyValue::Number(n))
}

/// A known bool output.
pub fn bool(b: bool) -> Output<PropertyValue> {
    Output::from_value(PropertyValue::Bool(b))
}

/// A known null output.
pub fn null() -> Output<PropertyValue> {
    Output::from_value(PropertyValue::Null)
}

/// An array output from element outputs. If any element is secret or
/// unknown, the array as a whole is.
pub fn array(items: Vec<Output<PropertyValue>>) -> Output<PropertyValue> {
    all(items).cast()
}

/// An object output from named field outputs. Field-level unknownness and
/// secretness stay attached to the fields.
pub fn object(fields: Vec<(String, Output<PropertyValue>)>) -> Output<PropertyValue> {
    crate::output::object(fields)
}

/// Interpolate outputs into a single string.
pub fn concat(parts: Vec<Output<PropertyValue>>) -> Output<PropertyValue> {
    crate::output::concat(parts).cast()
}

/// Mark a value secret.
pub fn secret(o: Output<PropertyValue>) -> Output<PropertyValue> {
    o.as_secret()
}

/// Remove secretness from a value.
pub fn unsecret(o: Output<PropertyValue>) -> Output<PropertyValue> {
    Output::from_data_future(async move {
        let mut d = o.data().await;
        d.secret = false;
        d
    })
}

/// A file asset.
pub fn file_asset(path: Output<PropertyValue>) -> Output<PropertyValue> {
    path.cast::<String>().map(|p| PropertyValue::Asset(crate::value::Asset::from_path(p)))
        .cast()
}

/// A string (literal text) asset.
pub fn string_asset(text: Output<PropertyValue>) -> Output<PropertyValue> {
    text.cast::<String>().map(|t| PropertyValue::Asset(crate::value::Asset::from_text(t))).cast()
}

/// A remote asset.
pub fn remote_asset(uri: Output<PropertyValue>) -> Output<PropertyValue> {
    uri.cast::<String>().map(|u| PropertyValue::Asset(crate::value::Asset::from_uri(u))).cast()
}

/// A file archive.
pub fn file_archive(path: Output<PropertyValue>) -> Output<PropertyValue> {
    path.cast::<String>().map(|p| PropertyValue::Archive(crate::value::Archive::from_path(p)))
        .cast()
}

/// A remote archive.
pub fn remote_archive(uri: Output<PropertyValue>) -> Output<PropertyValue> {
    uri.cast::<String>().map(|u| PropertyValue::Archive(crate::value::Archive::from_uri(u))).cast()
}

/// An asset archive built from a map of assets/archives.
pub fn asset_archive(entries: Vec<(String, Output<PropertyValue>)>) -> Output<PropertyValue> {
    object(entries).cast::<crate::value::PropertyMap>().map(|m| {
        let mut assets = std::collections::BTreeMap::new();
        for (k, v) in m {
            match v {
                PropertyValue::Asset(a) => {
                    assets.insert(k, crate::value::AssetOrArchive::Asset(a));
                }
                PropertyValue::Archive(a) => {
                    assets.insert(k, crate::value::AssetOrArchive::Archive(a));
                }
                _ => {}
            }
        }
        PropertyValue::Archive(crate::value::Archive::from_assets(assets))
    })
    .cast()
}

/// The current working directory (PCL `cwd()`).
pub fn cwd() -> Output<PropertyValue> {
    let dir = std::env::current_dir()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    string(dir)
}

/// Read a file's contents as a string (PCL `readFile`).
pub fn read_file(path: Output<PropertyValue>) -> Output<PropertyValue> {
    path.cast::<String>().map(|p: String| std::fs::read_to_string(p).unwrap_or_default()).cast()
}

/// Base64-encode a string (PCL `toBase64`).
pub fn to_base64(v: Output<PropertyValue>) -> Output<PropertyValue> {
    use base64::Engine;
    v.cast::<String>().map(|s| base64::engine::general_purpose::STANDARD.encode(s)).cast()
}

/// Base64-decode a string (PCL `fromBase64`).
pub fn from_base64(v: Output<PropertyValue>) -> Output<PropertyValue> {
    use base64::Engine;
    v.cast::<String>().map(|s| {
        base64::engine::general_purpose::STANDARD
            .decode(s)
            .ok()
            .and_then(|b| String::from_utf8(b).ok())
            .unwrap_or_default()
    })
    .cast()
}

/// Serialize a value to JSON (PCL `toJSON`).
pub fn to_json(v: Output<PropertyValue>) -> Output<PropertyValue> {
    v.map(|p: PropertyValue| property_to_json_string(&p)).cast()
}

fn property_to_json(v: &PropertyValue) -> serde_json::Value {
    match v {
        PropertyValue::Null | PropertyValue::Computed => serde_json::Value::Null,
        PropertyValue::Bool(b) => serde_json::Value::Bool(*b),
        PropertyValue::Number(n) => serde_json::Number::from_f64(*n)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        PropertyValue::String(s) => serde_json::Value::String(s.clone()),
        PropertyValue::Array(a) => {
            serde_json::Value::Array(a.iter().map(property_to_json).collect())
        }
        PropertyValue::Object(m) => serde_json::Value::Object(
            m.iter().map(|(k, v)| (k.clone(), property_to_json(v))).collect(),
        ),
        PropertyValue::Secret(inner) => property_to_json(inner),
        PropertyValue::Output(o) => match &o.value {
            Some(inner) => property_to_json(inner),
            None => serde_json::Value::Null,
        },
        _ => serde_json::Value::Null,
    }
}

fn property_to_json_string(v: &PropertyValue) -> String {
    serde_json::to_string(&property_to_json(v)).unwrap_or_default()
}

/// Join a list of strings with a separator (PCL `join`).
pub fn join(sep: Output<PropertyValue>, list: Output<PropertyValue>) -> Output<PropertyValue> {
    array(vec![sep, list])
        .cast::<Vec<PropertyValue>>().map(|vals| {
            let sep = match &vals[0] {
                PropertyValue::String(s) => s.clone(),
                _ => String::new(),
            };
            let parts: Vec<String> = match &vals[1] {
                PropertyValue::Array(a) => a
                    .iter()
                    .map(|v| match v {
                        PropertyValue::String(s) => s.clone(),
                        other => format!("{other:?}"),
                    })
                    .collect(),
                _ => vec![],
            };
            parts.join(&sep)
        })
        .cast()
}

/// The length of a string, list, or map (PCL `length`).
pub fn length(v: Output<PropertyValue>) -> Output<PropertyValue> {
    v.map(|p: PropertyValue| {
        let n = match &p {
            PropertyValue::String(s) => s.chars().count(),
            PropertyValue::Array(a) => a.len(),
            PropertyValue::Object(m) => m.len(),
            _ => 0,
        };
        n as f64
    })
    .cast()
}

/// Split a string (PCL `split`).
pub fn split(sep: Output<PropertyValue>, s: Output<PropertyValue>) -> Output<PropertyValue> {
    array(vec![sep, s])
        .cast::<Vec<PropertyValue>>().map(|vals| {
            let sep = match &vals[0] {
                PropertyValue::String(s) => s.clone(),
                _ => String::new(),
            };
            let s = match &vals[1] {
                PropertyValue::String(s) => s.clone(),
                _ => String::new(),
            };
            PropertyValue::Array(
                s.split(&sep).map(|p| PropertyValue::String(p.to_string())).collect(),
            )
        })
        .cast()
}

/// Retrieve an element of a list (PCL `element`).
pub fn element(list: Output<PropertyValue>, idx: Output<PropertyValue>) -> Output<PropertyValue> {
    crate::ops::index(list, idx)
}

/// A [key, value] entry list of an object or list (PCL `entries`).
pub fn entries(v: Output<PropertyValue>) -> Output<PropertyValue> {
    v.map(|p: PropertyValue| match p {
        PropertyValue::Object(m) => PropertyValue::Array(
            m.into_iter()
                .map(|(k, v)| {
                    let mut e = std::collections::BTreeMap::new();
                    e.insert("key".to_string(), PropertyValue::String(k));
                    e.insert("value".to_string(), v);
                    PropertyValue::Object(e)
                })
                .collect(),
        ),
        PropertyValue::Array(a) => PropertyValue::Array(
            a.into_iter()
                .enumerate()
                .map(|(i, v)| {
                    let mut e = std::collections::BTreeMap::new();
                    e.insert("key".to_string(), PropertyValue::Number(i as f64));
                    e.insert("value".to_string(), v);
                    PropertyValue::Object(e)
                })
                .collect(),
        ),
        other => other,
    })
    .cast()
}
