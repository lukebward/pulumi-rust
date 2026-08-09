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
        PropertyValue::Number(n) => json_number(*n),
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

/// Render a property number the way the other Pulumi SDKs do.
///
/// Every Pulumi number is a float on the wire, but Go's `encoding/json` — and
/// so every language whose `toJSON` goes through it — writes a whole number
/// without a fractional part. `serde_json::Number::from_f64` keeps the float
/// representation, so a port that ignores this emits `"containerPort": 80.0`,
/// which APIs expecting an integer reject. Above 2^53 a float can no longer
/// represent every integer, so those keep the float form.
fn json_number(n: f64) -> serde_json::Value {
    const MAX_EXACT_INT: f64 = 9_007_199_254_740_992.0; // 2^53
    if n.fract() == 0.0 && n.abs() <= MAX_EXACT_INT {
        return serde_json::Value::Number(serde_json::Number::from(n as i64));
    }
    serde_json::Number::from_f64(n)
        .map(serde_json::Value::Number)
        .unwrap_or(serde_json::Value::Null)
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

/// One iteration of a resource `range` option.
#[derive(Clone, Debug)]
pub struct RangeEntry {
    /// The iteration key: the index for counts and lists, the map key for
    /// maps, and null for a boolean range.
    pub key: PropertyValue,
    /// The iteration value.
    pub value: PropertyValue,
}

impl RangeEntry {
    /// The key rendered for use in resource names and lookups.
    pub fn key_string(&self) -> String {
        match &self.key {
            PropertyValue::String(s) => s.clone(),
            PropertyValue::Number(n) if n.fract() == 0.0 && n.abs() < 1e15 => {
                format!("{}", *n as i64)
            }
            PropertyValue::Number(n) => n.to_string(),
            PropertyValue::Null => String::new(),
            other => format!("{other:?}"),
        }
    }

    /// The resource name for this iteration: the declared name suffixed with
    /// the iteration key, except for boolean ranges which produce at most one
    /// resource and keep the bare name.
    pub fn name(&self, base: &str) -> String {
        match &self.key {
            PropertyValue::Null => base.to_string(),
            _ => format!("{}-{}", base, self.key_string()),
        }
    }
}

/// Expand a `range` option into the iterations it describes: a bool creates
/// zero or one resource, a count creates that many indexed from zero, a list
/// iterates by index, and a map iterates by key.
pub async fn range_entries(r: Output<PropertyValue>) -> Vec<RangeEntry> {
    let entry = |key: PropertyValue, value: PropertyValue| RangeEntry { key, value };
    match strip_wrappers(&r.data().await.value) {
        PropertyValue::Bool(true) => vec![entry(PropertyValue::Null, PropertyValue::Bool(true))],
        PropertyValue::Number(n) if n > 0.0 => (0..n as i64)
            .map(|i| entry(PropertyValue::Number(i as f64), PropertyValue::Number(i as f64)))
            .collect(),
        PropertyValue::Array(items) => items
            .into_iter()
            .enumerate()
            .map(|(i, v)| entry(PropertyValue::Number(i as f64), v))
            .collect(),
        PropertyValue::Object(m) => {
            m.into_iter().map(|(k, v)| entry(PropertyValue::String(k), v)).collect()
        }
        // A false bool, a zero/negative count, or an unknown range (during a
        // preview) all create nothing.
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn value(o: Output<PropertyValue>) -> PropertyValue {
        o.data().await.value
    }

    fn s(v: &str) -> Output<PropertyValue> {
        string(v)
    }

    // --- toJSON ------------------------------------------------------------

    #[tokio::test]
    async fn to_json_writes_whole_numbers_without_a_fraction() {
        // Every Pulumi number is a float on the wire, but `toJSON` in the
        // other SDKs goes through Go's encoder, which writes 80 not 80.0.
        // APIs that want an integer port reject the latter.
        let v = to_json(number(80.0));
        assert_eq!(value(v).await, PropertyValue::String("80".into()));
    }

    #[tokio::test]
    async fn to_json_keeps_a_real_fraction() {
        let v = to_json(number(1.5));
        assert_eq!(value(v).await, PropertyValue::String("1.5".into()));
    }

    #[tokio::test]
    async fn to_json_nests_numbers_correctly() {
        let v = to_json(object(vec![
            ("port".to_string(), number(80.0)),
            ("ratio".to_string(), number(0.5)),
        ]));
        assert_eq!(
            value(v).await,
            PropertyValue::String(r#"{"port":80,"ratio":0.5}"#.into())
        );
    }

    #[tokio::test]
    async fn to_json_is_secret_if_anything_inside_is() {
        // The secret is nested, so it is only visible via contains_secret —
        // a JSON document built from a secret is itself a secret.
        let d = to_json(object(vec![("k".to_string(), secret(s("shh")))])).data().await;
        assert!(d.secret);
    }

    #[tokio::test]
    async fn to_json_short_circuits_on_unknown() {
        let d = to_json(array(vec![s("a"), Output::unknown()])).data().await;
        assert!(!d.known());
    }

    // --- collections and strings -------------------------------------------

    #[tokio::test]
    async fn join_and_split_round_trip() {
        let joined = join(s(","), array(vec![s("a"), s("b"), s("c")]));
        assert_eq!(value(joined.clone()).await, PropertyValue::String("a,b,c".into()));
        let back = split(s(","), joined);
        assert_eq!(
            value(back).await,
            PropertyValue::Array(vec![
                PropertyValue::String("a".into()),
                PropertyValue::String("b".into()),
                PropertyValue::String("c".into()),
            ])
        );
    }

    #[tokio::test]
    async fn length_counts_arrays_objects_and_strings() {
        assert_eq!(value(length(array(vec![s("a"), s("b")]))).await, PropertyValue::Number(2.0));
        assert_eq!(value(length(s("abcd"))).await, PropertyValue::Number(4.0));
        assert_eq!(
            value(length(object(vec![("k".to_string(), s("v"))]))).await,
            PropertyValue::Number(1.0)
        );
    }

    #[tokio::test]
    async fn lookup_falls_back_to_the_default() {
        let m = object(vec![("present".to_string(), s("yes"))]);
        assert_eq!(value(lookup(m.clone(), s("present"), s("dflt"))).await,
                   PropertyValue::String("yes".into()));
        assert_eq!(value(lookup(m, s("absent"), s("dflt"))).await,
                   PropertyValue::String("dflt".into()));
    }

    #[tokio::test]
    async fn single_or_none_handles_all_three_cases() {
        assert_eq!(value(single_or_none(array(vec![s("x")]))).await,
                   PropertyValue::String("x".into()));
        assert_eq!(value(single_or_none(array(vec![]))).await, PropertyValue::Null);
    }

    #[tokio::test]
    async fn entries_of_an_object_are_key_value_pairs_in_key_order() {
        let v = entries(object(vec![
            ("b".to_string(), number(2.0)),
            ("a".to_string(), number(1.0)),
        ]));
        match value(v).await {
            PropertyValue::Array(items) => {
                assert_eq!(items.len(), 2);
                // BTreeMap ordering: "a" before "b", so iteration is stable
                // across runs — resource names derived from it must not churn.
                let first = match &items[0] {
                    PropertyValue::Object(m) => m.get("key").cloned(),
                    _ => None,
                };
                assert_eq!(first, Some(PropertyValue::String("a".into())));
            }
            other => panic!("expected an array, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn entries_of_an_array_key_by_index() {
        let v = entries(array(vec![s("x")]));
        match value(v).await {
            PropertyValue::Array(items) => match &items[0] {
                PropertyValue::Object(m) => {
                    assert_eq!(m.get("key"), Some(&PropertyValue::Number(0.0)));
                    assert_eq!(m.get("value"), Some(&PropertyValue::String("x".into())));
                }
                other => panic!("expected an object, got {other:?}"),
            },
            other => panic!("expected an array, got {other:?}"),
        }
    }

    // --- base64 / hashing --------------------------------------------------

    #[tokio::test]
    async fn base64_round_trips() {
        let encoded = to_base64(s("hello"));
        assert_eq!(value(encoded.clone()).await, PropertyValue::String("aGVsbG8=".into()));
        assert_eq!(value(from_base64(encoded)).await, PropertyValue::String("hello".into()));
    }

    #[tokio::test]
    async fn sha1_matches_the_known_digest() {
        assert_eq!(
            value(sha1_hex(s("abc"))).await,
            PropertyValue::String("a9993e364706816aba3e25717850c26c9cd0d89d".into())
        );
    }

    // --- secrecy -----------------------------------------------------------

    #[tokio::test]
    async fn secret_and_unsecret_are_inverses() {
        let d = secret(s("x")).data().await;
        assert!(d.secret);
        let d = unsecret(secret(s("x"))).data().await;
        assert!(!d.secret);
    }

    #[tokio::test]
    async fn an_array_of_a_secret_keeps_the_secret_on_the_element() {
        // `all` deliberately leaves the array itself non-secret and encodes
        // element secretness inline, so a partially-secret list round-trips.
        let d = array(vec![s("plain"), secret(s("shh"))]).data().await;
        assert!(d.value.contains_secret());
    }

    // --- urn helpers -------------------------------------------------------

    #[tokio::test]
    async fn urn_name_and_type_split_a_urn() {
        let urn = s("urn:pulumi:dev::proj::simple:index:Resource::res");
        assert_eq!(value(urn_name(urn.clone())).await, PropertyValue::String("res".into()));
        assert_eq!(
            value(urn_type(urn)).await,
            PropertyValue::String("simple:index:Resource".into())
        );
    }

    // --- range -------------------------------------------------------------

    #[tokio::test]
    async fn range_of_a_count_indexes_from_zero() {
        let entries = range_entries(number(3.0)).await;
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].key_string(), "0");
        assert_eq!(entries[2].name("web"), "web-2");
    }

    #[tokio::test]
    async fn range_of_a_bool_creates_at_most_one_unsuffixed_resource() {
        let entries = range_entries(bool(true)).await;
        assert_eq!(entries.len(), 1);
        // A boolean range names the single resource plainly — no "-0".
        assert_eq!(entries[0].name("web"), "web");
        assert!(range_entries(bool(false)).await.is_empty());
    }

    #[tokio::test]
    async fn range_of_a_map_iterates_by_key() {
        let entries = range_entries(object(vec![
            ("b".to_string(), number(2.0)),
            ("a".to_string(), number(1.0)),
        ]))
        .await;
        let names: Vec<String> = entries.iter().map(|e| e.name("web")).collect();
        assert_eq!(names, vec!["web-a".to_string(), "web-b".to_string()]);
    }

    #[tokio::test]
    async fn an_unknown_range_creates_nothing() {
        // During a preview the range may be unknown; creating resources from
        // it would register names that cannot be reproduced on the update.
        assert!(range_entries(Output::unknown()).await.is_empty());
        assert!(range_entries(number(0.0)).await.is_empty());
        assert!(range_entries(number(-1.0)).await.is_empty());
    }
}
