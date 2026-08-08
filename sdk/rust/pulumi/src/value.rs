//! Pulumi property values and their protobuf wire encoding.
//!
//! Property bags cross the wire as `google.protobuf.Struct` values with a few
//! special object encodings distinguished by a well-known signature key. The
//! constants and rules here mirror `sdk/go/common/resource/plugin/rpc.go` and
//! `sdk/go/common/resource/sig/sig.go` in pulumi/pulumi.

use std::collections::BTreeMap;

use prost_types::value::Kind;
use prost_types::{ListValue, Struct, Value};

/// The signature key used to encode type identity inside a map.
pub const SIG_KEY: &str = "4dabf18193072939515e22adb298388d";
/// The unique secret signature.
pub const SECRET_SIG: &str = "1b47061264138c4ac30d75fd1eb44270";
/// The unique resource reference signature.
pub const RESOURCE_REFERENCE_SIG: &str = "5cf8f73096256a8f31e491e813e4eb8e";
/// The unique output value signature.
pub const OUTPUT_VALUE_SIG: &str = "d0e6a833031e9bbcd3f4e8bde6ca49a4";
/// The unique signature for strings holding non-UTF8 bytes.
pub const BYTE_STRING_SIG: &str = "803fd3297a5875dc03ca845dda5d2a98";
/// The unique asset signature.
pub const ASSET_SIG: &str = "c44067f5952c0a294b673a41bacd8c17";
/// The unique archive signature.
pub const ARCHIVE_SIG: &str = "0def7320c3a5731c473e5ecbe6d01bc7";

/// Sentinel indicating an unknown string (and the generic unknown sentinel).
pub const UNKNOWN_STRING_VALUE: &str = "04da6b54-80e4-46f7-96ec-b56ff0331ba9";
/// Sentinel indicating an unknown bool.
pub const UNKNOWN_BOOL_VALUE: &str = "1c4a061d-8072-4f0a-a4cb-0ff528b18fe7";
/// Sentinel indicating an unknown number.
pub const UNKNOWN_NUMBER_VALUE: &str = "3eeb2bf0-c639-47a8-9e75-3b44932eb421";
/// Sentinel indicating an unknown array.
pub const UNKNOWN_ARRAY_VALUE: &str = "6a19a0b0-7e62-4c92-b797-7f8e31da9cc2";
/// Sentinel indicating an unknown asset.
pub const UNKNOWN_ASSET_VALUE: &str = "030794c1-ac77-496b-92df-f27374a8bd58";
/// Sentinel indicating an unknown archive.
pub const UNKNOWN_ARCHIVE_VALUE: &str = "e48ece36-62e2-4504-bad9-02848725956a";
/// Sentinel indicating an unknown object.
pub const UNKNOWN_OBJECT_VALUE: &str = "dd056dcd-154b-4c76-9bd3-c8f88648b5ff";

/// An asset: textual or binary content addressed by literal text, a local
/// path, or a remote URI.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Asset {
    pub hash: Option<String>,
    pub text: Option<String>,
    pub path: Option<String>,
    pub uri: Option<String>,
}

impl Asset {
    pub fn from_text(text: impl Into<String>) -> Self {
        Asset { text: Some(text.into()), ..Default::default() }
    }

    pub fn from_path(path: impl Into<String>) -> Self {
        Asset { path: Some(path.into()), ..Default::default() }
    }

    pub fn from_uri(uri: impl Into<String>) -> Self {
        Asset { uri: Some(uri.into()), ..Default::default() }
    }
}

/// One entry of an [`Archive`]'s asset map.
#[derive(Clone, Debug, PartialEq)]
pub enum AssetOrArchive {
    Asset(Asset),
    Archive(Archive),
}

/// An archive: a collection of assets addressed by a local path, a remote
/// URI, or a literal map of named assets.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Archive {
    pub hash: Option<String>,
    pub assets: Option<BTreeMap<String, AssetOrArchive>>,
    pub path: Option<String>,
    pub uri: Option<String>,
}

impl Archive {
    pub fn from_path(path: impl Into<String>) -> Self {
        Archive { path: Some(path.into()), ..Default::default() }
    }

    pub fn from_uri(uri: impl Into<String>) -> Self {
        Archive { uri: Some(uri.into()), ..Default::default() }
    }

    pub fn from_assets(assets: BTreeMap<String, AssetOrArchive>) -> Self {
        Archive { assets: Some(assets), ..Default::default() }
    }
}

/// A strongly typed reference to another resource.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ResourceReference {
    pub urn: String,
    /// The resource ID for custom resources; `None` for component resources.
    /// `Some(None)` means a custom resource whose ID is unknown.
    pub id: Option<Option<String>>,
    pub package_version: String,
}

/// A Pulumi property value.
///
/// This is the dynamic value model every input and output ultimately flows
/// through. `Computed` is a bare unknown; `Secret` wraps a value that must be
/// encrypted at rest; `Output` is the rich form carrying unknownness,
/// secretness, and dependency URNs together.
#[derive(Clone, Debug, PartialEq)]
pub enum PropertyValue {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    /// A string whose bytes are not valid UTF-8. Rust's `String` cannot hold
    /// these, so they travel as raw bytes and marshal with the byte-string
    /// signature.
    ByteString(Vec<u8>),
    Array(Vec<PropertyValue>),
    Object(BTreeMap<String, PropertyValue>),
    Asset(Asset),
    Archive(Archive),
    Secret(Box<PropertyValue>),
    Computed,
    Output(OutputValue),
    ResourceReference(ResourceReference),
    /// An internal sentinel for a lookup that found nothing, as distinct
    /// from a property explicitly set to null. Only `try` and `can` observe
    /// it; everywhere else, including the wire, it behaves as null.
    Missing,
    /// An internal sentinel for a value whose resource registration failed,
    /// carrying the failure message. Only `recover`, `try` and `can`
    /// observe it; on the wire it behaves as null.
    Failed(std::sync::Arc<str>),
}

/// The payload of a first-class output value on the wire.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputValue {
    /// The value, if known.
    pub value: Option<Box<PropertyValue>>,
    pub secret: bool,
    /// URNs of the resources this value depends on.
    pub dependencies: Vec<String>,
}

/// A property bag.
pub type PropertyMap = BTreeMap<String, PropertyValue>;

fn string_value(s: impl Into<String>) -> Value {
    Value { kind: Some(Kind::StringValue(s.into())) }
}

fn sig_object(sig: &str) -> BTreeMap<String, Value> {
    let mut m = BTreeMap::new();
    m.insert(SIG_KEY.to_string(), string_value(sig));
    m
}

fn struct_from(fields: BTreeMap<String, Value>) -> Struct {
    Struct { fields }
}

fn object_value(fields: BTreeMap<String, Value>) -> Value {
    Value { kind: Some(Kind::StructValue(struct_from(fields))) }
}

impl Asset {
    fn to_proto(&self) -> Value {
        let mut m = sig_object(ASSET_SIG);
        if let Some(h) = &self.hash {
            m.insert("hash".into(), string_value(h.clone()));
        }
        if let Some(t) = &self.text {
            m.insert("text".into(), string_value(t.clone()));
        }
        if let Some(p) = &self.path {
            m.insert("path".into(), string_value(p.clone()));
        }
        if let Some(u) = &self.uri {
            m.insert("uri".into(), string_value(u.clone()));
        }
        object_value(m)
    }
}

impl Archive {
    fn to_proto(&self) -> Value {
        let mut m = sig_object(ARCHIVE_SIG);
        if let Some(h) = &self.hash {
            m.insert("hash".into(), string_value(h.clone()));
        }
        if let Some(assets) = &self.assets {
            let mut am = BTreeMap::new();
            for (k, v) in assets {
                let val = match v {
                    AssetOrArchive::Asset(a) => a.to_proto(),
                    AssetOrArchive::Archive(a) => a.to_proto(),
                };
                am.insert(k.clone(), val);
            }
            m.insert("assets".into(), object_value(am));
        }
        if let Some(p) = &self.path {
            m.insert("path".into(), string_value(p.clone()));
        }
        if let Some(u) = &self.uri {
            m.insert("uri".into(), string_value(u.clone()));
        }
        object_value(m)
    }
}

impl PropertyValue {
    /// True if this value or anything nested inside it is unknown.
    pub fn contains_unknown(&self) -> bool {
        match self {
            PropertyValue::Computed => true,
            PropertyValue::Secret(v) => v.contains_unknown(),
            PropertyValue::Output(o) => match &o.value {
                None => true,
                Some(v) => v.contains_unknown(),
            },
            PropertyValue::Array(vs) => vs.iter().any(|v| v.contains_unknown()),
            PropertyValue::Object(m) => m.values().any(|v| v.contains_unknown()),
            _ => false,
        }
    }

    /// True if this value or anything nested inside it is secret.
    pub fn contains_secret(&self) -> bool {
        match self {
            PropertyValue::Secret(_) => true,
            PropertyValue::Output(o) => {
                o.secret || o.value.as_deref().is_some_and(|v| v.contains_secret())
            }
            PropertyValue::Array(vs) => vs.iter().any(|v| v.contains_secret()),
            PropertyValue::Object(m) => m.values().any(|v| v.contains_secret()),
            _ => false,
        }
    }

    /// Collect every dependency URN mentioned by nested output values.
    pub fn collect_dependencies(&self, into: &mut Vec<String>) {
        match self {
            PropertyValue::Secret(v) => v.collect_dependencies(into),
            PropertyValue::Output(o) => {
                into.extend(o.dependencies.iter().cloned());
                if let Some(v) = &o.value {
                    v.collect_dependencies(into);
                }
            }
            PropertyValue::Array(vs) => {
                for v in vs {
                    v.collect_dependencies(into);
                }
            }
            PropertyValue::Object(m) => {
                for v in m.values() {
                    v.collect_dependencies(into);
                }
            }
            _ => {}
        }
    }

    /// Marshal to the protobuf wire form, keeping unknowns, secrets, resource
    /// references, and output values (the modern feature set the conformance
    /// engine negotiates).
    pub fn to_proto(&self) -> Value {
        match self {
            PropertyValue::Null | PropertyValue::Missing | PropertyValue::Failed(_) => {
                Value { kind: Some(Kind::NullValue(0)) }
            }
            PropertyValue::Bool(b) => Value { kind: Some(Kind::BoolValue(*b)) },
            PropertyValue::Number(n) => Value { kind: Some(Kind::NumberValue(*n)) },
            PropertyValue::String(s) => string_value(s.clone()),
            PropertyValue::ByteString(bytes) => {
                use base64::Engine;
                let mut m = sig_object(BYTE_STRING_SIG);
                m.insert(
                    "value".into(),
                    string_value(base64::engine::general_purpose::STANDARD.encode(bytes)),
                );
                object_value(m)
            }
            PropertyValue::Array(vs) => Value {
                kind: Some(Kind::ListValue(ListValue {
                    values: vs.iter().map(|v| v.to_proto()).collect(),
                })),
            },
            PropertyValue::Object(m) => {
                let fields = m.iter().map(|(k, v)| (k.clone(), v.to_proto())).collect();
                object_value(fields)
            }
            PropertyValue::Asset(a) => a.to_proto(),
            PropertyValue::Archive(a) => a.to_proto(),
            PropertyValue::Secret(v) => {
                let mut m = sig_object(SECRET_SIG);
                m.insert("value".into(), v.to_proto());
                object_value(m)
            }
            PropertyValue::Computed => string_value(UNKNOWN_STRING_VALUE),
            PropertyValue::Output(o) => {
                let mut m = sig_object(OUTPUT_VALUE_SIG);
                if let Some(v) = &o.value {
                    m.insert("value".into(), v.to_proto());
                }
                if o.secret {
                    m.insert("secret".into(), Value { kind: Some(Kind::BoolValue(true)) });
                }
                if !o.dependencies.is_empty() {
                    m.insert(
                        "dependencies".into(),
                        Value {
                            kind: Some(Kind::ListValue(ListValue {
                                values: o
                                    .dependencies
                                    .iter()
                                    .map(|d| string_value(d.clone()))
                                    .collect(),
                            })),
                        },
                    );
                }
                object_value(m)
            }
            PropertyValue::ResourceReference(r) => {
                let mut m = sig_object(RESOURCE_REFERENCE_SIG);
                m.insert("urn".into(), string_value(r.urn.clone()));
                match &r.id {
                    Some(Some(id)) => {
                        m.insert("id".into(), string_value(id.clone()));
                    }
                    Some(None) => {
                        m.insert("id".into(), string_value(UNKNOWN_STRING_VALUE));
                    }
                    None => {}
                }
                if !r.package_version.is_empty() {
                    m.insert("packageVersion".into(), string_value(r.package_version.clone()));
                }
                object_value(m)
            }
        }
    }

    /// Unmarshal from the protobuf wire form.
    pub fn from_proto(v: &Value) -> PropertyValue {
        match &v.kind {
            None | Some(Kind::NullValue(_)) => PropertyValue::Null,
            Some(Kind::BoolValue(b)) => PropertyValue::Bool(*b),
            Some(Kind::NumberValue(n)) => PropertyValue::Number(*n),
            Some(Kind::StringValue(s)) => {
                if s == UNKNOWN_STRING_VALUE
                    || s == UNKNOWN_BOOL_VALUE
                    || s == UNKNOWN_NUMBER_VALUE
                    || s == UNKNOWN_ARRAY_VALUE
                    || s == UNKNOWN_ASSET_VALUE
                    || s == UNKNOWN_ARCHIVE_VALUE
                    || s == UNKNOWN_OBJECT_VALUE
                {
                    PropertyValue::Computed
                } else {
                    PropertyValue::String(s.clone())
                }
            }
            Some(Kind::ListValue(l)) => {
                PropertyValue::Array(l.values.iter().map(PropertyValue::from_proto).collect())
            }
            Some(Kind::StructValue(s)) => Self::from_proto_struct(s),
        }
    }

    fn from_proto_struct(s: &Struct) -> PropertyValue {
        let sig = s.fields.get(SIG_KEY).and_then(|v| match &v.kind {
            Some(Kind::StringValue(s)) => Some(s.as_str()),
            _ => None,
        });
        let get_string = |key: &str| -> Option<String> {
            s.fields.get(key).and_then(|v| match &v.kind {
                Some(Kind::StringValue(s)) => Some(s.clone()),
                _ => None,
            })
        };
        match sig {
            Some(SECRET_SIG) => {
                let inner = s
                    .fields
                    .get("value")
                    .map(PropertyValue::from_proto)
                    .unwrap_or(PropertyValue::Null);
                PropertyValue::Secret(Box::new(inner))
            }
            Some(OUTPUT_VALUE_SIG) => {
                let value = s.fields.get("value").map(|v| Box::new(PropertyValue::from_proto(v)));
                let secret = s
                    .fields
                    .get("secret")
                    .and_then(|v| match &v.kind {
                        Some(Kind::BoolValue(b)) => Some(*b),
                        _ => None,
                    })
                    .unwrap_or(false);
                let dependencies = s
                    .fields
                    .get("dependencies")
                    .and_then(|v| match &v.kind {
                        Some(Kind::ListValue(l)) => Some(
                            l.values
                                .iter()
                                .filter_map(|d| match &d.kind {
                                    Some(Kind::StringValue(s)) => Some(s.clone()),
                                    _ => None,
                                })
                                .collect::<Vec<_>>(),
                        ),
                        _ => None,
                    })
                    .unwrap_or_default();
                PropertyValue::Output(OutputValue { value, secret, dependencies })
            }
            Some(RESOURCE_REFERENCE_SIG) => {
                let id = match get_string("id") {
                    Some(id) if id == UNKNOWN_STRING_VALUE => Some(None),
                    Some(id) => Some(Some(id)),
                    None => None,
                };
                PropertyValue::ResourceReference(ResourceReference {
                    urn: get_string("urn").unwrap_or_default(),
                    id,
                    package_version: get_string("packageVersion").unwrap_or_default(),
                })
            }
            Some(ASSET_SIG) => PropertyValue::Asset(Asset {
                hash: get_string("hash"),
                text: get_string("text"),
                path: get_string("path"),
                uri: get_string("uri"),
            }),
            Some(ARCHIVE_SIG) => {
                let assets = s.fields.get("assets").and_then(|v| match &v.kind {
                    Some(Kind::StructValue(am)) => {
                        let mut out = BTreeMap::new();
                        for (k, av) in &am.fields {
                            match PropertyValue::from_proto(av) {
                                PropertyValue::Asset(a) => {
                                    out.insert(k.clone(), AssetOrArchive::Asset(a));
                                }
                                PropertyValue::Archive(a) => {
                                    out.insert(k.clone(), AssetOrArchive::Archive(a));
                                }
                                _ => {}
                            }
                        }
                        Some(out)
                    }
                    _ => None,
                });
                PropertyValue::Archive(Archive {
                    hash: get_string("hash"),
                    assets,
                    path: get_string("path"),
                    uri: get_string("uri"),
                })
            }
            Some(BYTE_STRING_SIG) => {
                // Byte strings arrive base64 encoded. Valid UTF-8 becomes an
                // ordinary string; anything else keeps its exact bytes.
                use base64::Engine;
                let decoded = get_string("value")
                    .and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok())
                    .unwrap_or_default();
                PropertyValue::from_bytes(decoded)
            }
            _ => {
                let fields =
                    s.fields.iter().map(|(k, v)| (k.clone(), PropertyValue::from_proto(v))).collect();
                PropertyValue::Object(fields)
            }
        }
    }
}

impl PropertyValue {
    /// Build a string-ish value from raw bytes, preferring the ordinary
    /// `String` form so `String` and `ByteString` never hold the same text.
    pub fn from_bytes(bytes: Vec<u8>) -> PropertyValue {
        match String::from_utf8(bytes) {
            Ok(s) => PropertyValue::String(s),
            Err(e) => PropertyValue::ByteString(e.into_bytes()),
        }
    }
}

/// Marshal a property map to a protobuf `Struct`.
pub fn marshal_properties(props: &PropertyMap) -> Struct {
    struct_from(props.iter().map(|(k, v)| (k.clone(), v.to_proto())).collect())
}

/// Unmarshal a protobuf `Struct` into a property map.
pub fn unmarshal_properties(s: &Struct) -> PropertyMap {
    s.fields.iter().map(|(k, v)| (k.clone(), PropertyValue::from_proto(v))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_scalars() {
        let vals = [
            PropertyValue::Null,
            PropertyValue::Bool(true),
            PropertyValue::Number(41.5),
            PropertyValue::String("hello".into()),
            PropertyValue::Computed,
            PropertyValue::Secret(Box::new(PropertyValue::String("shh".into()))),
        ];
        for v in vals {
            let rt = PropertyValue::from_proto(&v.to_proto());
            assert_eq!(rt, v);
        }
    }

    #[test]
    fn round_trip_output_value() {
        let v = PropertyValue::Output(OutputValue {
            value: Some(Box::new(PropertyValue::Number(3.0))),
            secret: true,
            dependencies: vec!["urn:pulumi:stack::proj::t::name".into()],
        });
        assert_eq!(PropertyValue::from_proto(&v.to_proto()), v);
    }

    #[test]
    fn round_trip_asset_archive() {
        let mut assets = BTreeMap::new();
        assets.insert("a.txt".to_string(), AssetOrArchive::Asset(Asset::from_text("hi")));
        let v = PropertyValue::Archive(Archive::from_assets(assets));
        assert_eq!(PropertyValue::from_proto(&v.to_proto()), v);
    }
}
