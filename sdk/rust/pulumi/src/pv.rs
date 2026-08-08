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
            match strip_wrappers(&v) {
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

/// Base64-encode a file's raw bytes (PCL `filebase64`).
pub fn file_base64(path: Output<PropertyValue>) -> Output<PropertyValue> {
    use base64::Engine;
    path.cast::<String>()
        .map(|p: String| {
            let bytes = std::fs::read(p).unwrap_or_default();
            base64::engine::general_purpose::STANDARD.encode(bytes)
        })
        .cast()
}

/// Base64-encoded SHA-256 of a file's bytes (PCL `filebase64sha256`).
pub fn file_base64_sha256(path: Output<PropertyValue>) -> Output<PropertyValue> {
    use base64::Engine;
    use sha2::Digest;
    path.cast::<String>()
        .map(|p: String| {
            let bytes = std::fs::read(p).unwrap_or_default();
            let digest = sha2::Sha256::digest(&bytes);
            base64::engine::general_purpose::STANDARD.encode(digest)
        })
        .cast()
}

/// Hex-encoded SHA-1 of a string (PCL `sha1`).
pub fn sha1_hex(v: Output<PropertyValue>) -> Output<PropertyValue> {
    use sha1::Digest;
    v.cast::<String>()
        .map(|s: String| {
            let digest = sha1::Sha1::digest(s.as_bytes());
            digest.iter().map(|b| format!("{b:02x}")).collect::<String>()
        })
        .cast()
}

/// Serialize a value to JSON (PCL `toJSON`). The result is secret when
/// anything inside the value is.
pub fn to_json(v: Output<PropertyValue>) -> Output<PropertyValue> {
    Output::from_data_future(async move {
        let d = v.data().await;
        if !d.known() {
            return d;
        }
        let secret = d.secret || d.value.contains_secret();
        OutputData {
            value: PropertyValue::String(property_to_json_string(&d.value)),
            secret,
            deps: d.deps,
        }
    })
}

fn property_to_json(v: &PropertyValue) -> serde_json::Value {
    match v {
        PropertyValue::Null | PropertyValue::Computed => serde_json::Value::Null,
        // JSON strings are UTF-8 only, so this narrows like Go's encoder.
        PropertyValue::ByteString(b) => {
            serde_json::Value::String(String::from_utf8_lossy(b).into_owned())
        }
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

/// Strip transparent secret/output wrappers off a value.
fn strip_wrappers(v: &PropertyValue) -> PropertyValue {
    match v {
        PropertyValue::Secret(inner) => strip_wrappers(inner),
        PropertyValue::Output(o) => match &o.value {
            Some(inner) => strip_wrappers(inner),
            None => PropertyValue::Computed,
        },
        other => other.clone(),
    }
}

/// Join a list of strings with a separator (PCL `join`).
pub fn join(sep: Output<PropertyValue>, list: Output<PropertyValue>) -> Output<PropertyValue> {
    array(vec![sep, list])
        .cast::<Vec<PropertyValue>>().map(|vals| {
            let sep = match strip_wrappers(&vals[0]) {
                PropertyValue::String(s) => s,
                _ => String::new(),
            };
            let parts: Vec<String> = match &strip_wrappers(&vals[1]) {
                PropertyValue::Array(a) => a
                    .iter()
                    .map(|v| match strip_wrappers(v) {
                        PropertyValue::String(s) => s,
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
        use unicode_segmentation::UnicodeSegmentation;
        let n = match &p {
            PropertyValue::String(s) => s.graphemes(true).count(),
            PropertyValue::ByteString(b) => b.len(),
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
            let sep = match strip_wrappers(&vals[0]) {
                PropertyValue::String(s) => s,
                _ => String::new(),
            };
            let s = match strip_wrappers(&vals[1]) {
                PropertyValue::String(s) => s,
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

/// Unwrap the sole property of a scalar-returning invoke's result object.
pub fn single_value(v: Output<PropertyValue>) -> Output<PropertyValue> {
    v.map(|p: PropertyValue| match p {
        PropertyValue::Object(m) if m.len() == 1 => m.into_iter().next().unwrap().1,
        other => other,
    })
    .cast()
}

/// The single element of a one-element list, or null (PCL `singleOrNone`).
pub fn single_or_none(v: Output<PropertyValue>) -> Output<PropertyValue> {
    v.map(|p: PropertyValue| match p {
        PropertyValue::Array(a) if a.len() == 1 => strip_wrappers(&a[0]),
        PropertyValue::Array(a) if a.is_empty() => PropertyValue::Null,
        PropertyValue::Array(a) => {
            panic!("singleOrNone expected a list with at most one element, got {}", a.len())
        }
        _ => PropertyValue::Null,
    })
    .cast()
}

/// Look up a key in a map with a default (PCL `lookup`).
pub fn lookup(
    m: Output<PropertyValue>,
    key: Output<PropertyValue>,
    default: Output<PropertyValue>,
) -> Output<PropertyValue> {
    let found = crate::ops::index(m, key);
    Output::from_data_future(async move {
        let d = found.data().await;
        if matches!(d.value, PropertyValue::Null) {
            return default.data().await;
        }
        d
    })
}

/// The numeric minimum of the arguments (PCL `min`).
pub fn min(items: Vec<Output<PropertyValue>>) -> Output<PropertyValue> {
    fold_numbers(items, f64::INFINITY, |acc, n| if n < acc { n } else { acc })
}

/// The numeric maximum of the arguments (PCL `max`).
pub fn max(items: Vec<Output<PropertyValue>>) -> Output<PropertyValue> {
    fold_numbers(items, f64::NEG_INFINITY, |acc, n| if n > acc { n } else { acc })
}

fn fold_numbers(
    items: Vec<Output<PropertyValue>>,
    init: f64,
    f: impl Fn(f64, f64) -> f64 + Send + 'static,
) -> Output<PropertyValue> {
    array(items)
        .cast::<Vec<PropertyValue>>()
        .map(move |vals| {
            // Splat-expanded final arguments arrive as nested lists;
            // flatten one level so max([1, 2, 3]...) works.
            let mut flat = vec![];
            for v in vals {
                match strip_wrappers(&v) {
                    PropertyValue::Array(inner) => {
                        flat.extend(inner.iter().map(strip_wrappers))
                    }
                    other => flat.push(other),
                }
            }
            let mut acc = init;
            for v in flat {
                if let PropertyValue::Number(n) = v {
                    acc = f(acc, n);
                }
            }
            PropertyValue::Number(acc)
        })
        .cast()
}

/// The resource name embedded in a URN (PCL `pulumiResourceName`).
pub fn urn_name(urn: Output<PropertyValue>) -> Output<PropertyValue> {
    urn.cast::<String>()
        .map(|u: String| u.rsplit("::").next().unwrap_or_default().to_string())
        .cast()
}

/// The resource type token embedded in a URN (PCL `pulumiResourceType`).
pub fn urn_type(urn: Output<PropertyValue>) -> Output<PropertyValue> {
    urn.cast::<String>()
        .map(|u: String| {
            let parts: Vec<&str> = u.split("::").collect();
            let ty = parts.get(2).copied().unwrap_or_default();
            ty.rsplit('$').next().unwrap_or_default().to_string()
        })
        .cast()
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
