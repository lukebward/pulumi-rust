//! Conversions between Rust types and [`PropertyValue`]s.
//!
//! Generated SDK types implement these traits so typed values can flow
//! through the dynamic property-value layer that the engine protocol speaks.

use std::collections::{BTreeMap, HashMap};

use crate::error::{Error, Result};
use crate::value::{Archive, Asset, PropertyMap, PropertyValue};

/// Types that can be converted into a [`PropertyValue`].
pub trait IntoPropertyValue {
    fn into_property_value(self) -> PropertyValue;
}

/// Types that can be recovered from a [`PropertyValue`].
pub trait FromPropertyValue: Sized {
    fn from_property_value(v: PropertyValue) -> Result<Self>;
}

impl IntoPropertyValue for PropertyValue {
    fn into_property_value(self) -> PropertyValue {
        self
    }
}

impl FromPropertyValue for PropertyValue {
    fn from_property_value(v: PropertyValue) -> Result<Self> {
        Ok(v)
    }
}

impl IntoPropertyValue for bool {
    fn into_property_value(self) -> PropertyValue {
        PropertyValue::Bool(self)
    }
}

impl FromPropertyValue for bool {
    fn from_property_value(v: PropertyValue) -> Result<Self> {
        match unwrap(v)? {
            PropertyValue::Bool(b) => Ok(b),
            other => Err(mismatch("bool", &other)),
        }
    }
}

impl IntoPropertyValue for f64 {
    fn into_property_value(self) -> PropertyValue {
        PropertyValue::Number(self)
    }
}

impl FromPropertyValue for f64 {
    fn from_property_value(v: PropertyValue) -> Result<Self> {
        match unwrap(v)? {
            PropertyValue::Number(n) => Ok(n),
            other => Err(mismatch("number", &other)),
        }
    }
}

impl IntoPropertyValue for i32 {
    fn into_property_value(self) -> PropertyValue {
        PropertyValue::Number(self as f64)
    }
}

impl FromPropertyValue for i32 {
    fn from_property_value(v: PropertyValue) -> Result<Self> {
        match unwrap(v)? {
            PropertyValue::Number(n) => Ok(n as i32),
            other => Err(mismatch("integer", &other)),
        }
    }
}

impl IntoPropertyValue for i64 {
    fn into_property_value(self) -> PropertyValue {
        PropertyValue::Number(self as f64)
    }
}

impl FromPropertyValue for i64 {
    fn from_property_value(v: PropertyValue) -> Result<Self> {
        match unwrap(v)? {
            PropertyValue::Number(n) => Ok(n as i64),
            other => Err(mismatch("integer", &other)),
        }
    }
}

impl IntoPropertyValue for String {
    fn into_property_value(self) -> PropertyValue {
        PropertyValue::String(self)
    }
}

impl FromPropertyValue for String {
    fn from_property_value(v: PropertyValue) -> Result<Self> {
        match unwrap(v)? {
            PropertyValue::String(s) => Ok(s),
            PropertyValue::ByteString(_) => Err(Error::new(
                "cannot convert a string containing non-UTF8 bytes to a Rust String",
            )),
            other => Err(mismatch("string", &other)),
        }
    }
}

impl IntoPropertyValue for &str {
    fn into_property_value(self) -> PropertyValue {
        PropertyValue::String(self.to_string())
    }
}

impl IntoPropertyValue for Asset {
    fn into_property_value(self) -> PropertyValue {
        PropertyValue::Asset(self)
    }
}

impl FromPropertyValue for Asset {
    fn from_property_value(v: PropertyValue) -> Result<Self> {
        match unwrap(v)? {
            PropertyValue::Asset(a) => Ok(a),
            other => Err(mismatch("asset", &other)),
        }
    }
}

impl IntoPropertyValue for Archive {
    fn into_property_value(self) -> PropertyValue {
        PropertyValue::Archive(self)
    }
}

impl FromPropertyValue for Archive {
    fn from_property_value(v: PropertyValue) -> Result<Self> {
        match unwrap(v)? {
            PropertyValue::Archive(a) => Ok(a),
            other => Err(mismatch("archive", &other)),
        }
    }
}

impl IntoPropertyValue for crate::value::AssetOrArchive {
    fn into_property_value(self) -> PropertyValue {
        match self {
            crate::value::AssetOrArchive::Asset(a) => PropertyValue::Asset(a),
            crate::value::AssetOrArchive::Archive(a) => PropertyValue::Archive(a),
        }
    }
}

impl FromPropertyValue for crate::value::AssetOrArchive {
    fn from_property_value(v: PropertyValue) -> Result<Self> {
        match unwrap(v)? {
            PropertyValue::Asset(a) => Ok(crate::value::AssetOrArchive::Asset(a)),
            PropertyValue::Archive(a) => Ok(crate::value::AssetOrArchive::Archive(a)),
            other => Err(mismatch("asset or archive", &other)),
        }
    }
}

impl<T: IntoPropertyValue> IntoPropertyValue for Option<T> {
    fn into_property_value(self) -> PropertyValue {
        match self {
            Some(v) => v.into_property_value(),
            None => PropertyValue::Null,
        }
    }
}

impl<T: FromPropertyValue> FromPropertyValue for Option<T> {
    fn from_property_value(v: PropertyValue) -> Result<Self> {
        match v {
            PropertyValue::Null => Ok(None),
            other => Ok(Some(T::from_property_value(other)?)),
        }
    }
}

// A generated struct boxes a field that would otherwise make it infinitely
// sized — a schema object type that contains itself, like Kubernetes'
// `JSONSchemaProps.not`. The box is a Rust representation detail; on the
// wire the value is the same object, so both conversions step through it.
impl<T: IntoPropertyValue> IntoPropertyValue for Box<T> {
    fn into_property_value(self) -> PropertyValue {
        (*self).into_property_value()
    }
}

impl<T: FromPropertyValue> FromPropertyValue for Box<T> {
    fn from_property_value(v: PropertyValue) -> Result<Self> {
        Ok(Box::new(T::from_property_value(v)?))
    }
}

impl<T: IntoPropertyValue> IntoPropertyValue for Vec<T> {
    fn into_property_value(self) -> PropertyValue {
        PropertyValue::Array(self.into_iter().map(|v| v.into_property_value()).collect())
    }
}

impl<T: FromPropertyValue> FromPropertyValue for Vec<T> {
    fn from_property_value(v: PropertyValue) -> Result<Self> {
        match unwrap(v)? {
            PropertyValue::Array(vs) => {
                vs.into_iter().map(T::from_property_value).collect::<Result<Vec<_>>>()
            }
            other => Err(mismatch("array", &other)),
        }
    }
}

impl<T: IntoPropertyValue> IntoPropertyValue for BTreeMap<String, T> {
    fn into_property_value(self) -> PropertyValue {
        PropertyValue::Object(
            self.into_iter().map(|(k, v)| (k, v.into_property_value())).collect(),
        )
    }
}

impl<T: FromPropertyValue> FromPropertyValue for BTreeMap<String, T> {
    fn from_property_value(v: PropertyValue) -> Result<Self> {
        match unwrap(v)? {
            PropertyValue::Object(m) => m
                .into_iter()
                .map(|(k, v)| Ok((k, T::from_property_value(v)?)))
                .collect::<Result<BTreeMap<_, _>>>(),
            other => Err(mismatch("object", &other)),
        }
    }
}

impl<T: IntoPropertyValue> IntoPropertyValue for HashMap<String, T> {
    fn into_property_value(self) -> PropertyValue {
        PropertyValue::Object(
            self.into_iter().map(|(k, v)| (k, v.into_property_value())).collect(),
        )
    }
}

impl<T: FromPropertyValue> FromPropertyValue for HashMap<String, T> {
    fn from_property_value(v: PropertyValue) -> Result<Self> {
        match unwrap(v)? {
            PropertyValue::Object(m) => m
                .into_iter()
                .map(|(k, v)| Ok((k, T::from_property_value(v)?)))
                .collect::<Result<HashMap<_, _>>>(),
            other => Err(mismatch("object", &other)),
        }
    }
}

/// Convert a property map into a typed value keyed by property name.
pub fn from_property_map<T: FromPropertyValue>(m: &PropertyMap, key: &str) -> Result<T> {
    let v = m.get(key).cloned().unwrap_or(PropertyValue::Null);
    T::from_property_value(v)
}

/// Strip transparent wrappers (secret/output) so typed conversion sees the
/// plain value. Secretness and dependency tracking are handled at the
/// [`crate::output::Output`] layer before conversion happens.
fn unwrap(v: PropertyValue) -> Result<PropertyValue> {
    match v {
        PropertyValue::Secret(inner) => unwrap(*inner),
        PropertyValue::Output(o) => match o.value {
            Some(inner) => unwrap(*inner),
            None => Err(Error::new("cannot convert an unknown value")),
        },
        PropertyValue::Computed => Err(Error::new("cannot convert an unknown value")),
        other => Ok(other),
    }
}

fn mismatch(expected: &str, got: &PropertyValue) -> Error {
    Error::new(format!("expected {expected}, got {got:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip<T>(v: T) -> T
    where
        T: IntoPropertyValue + FromPropertyValue + Clone,
    {
        T::from_property_value(v.into_property_value()).expect("conversion failed")
    }

    #[test]
    fn scalars_round_trip() {
        assert_eq!(round_trip("hi".to_string()), "hi".to_string());
        assert_eq!(round_trip(42i64), 42i64);
        assert_eq!(round_trip(1.5f64), 1.5f64);
        assert!(round_trip(true));
    }

    #[test]
    fn an_option_maps_none_to_null_and_back() {
        assert_eq!(PropertyValue::Null, None::<String>.into_property_value());
        assert_eq!(
            <Option<String> as FromPropertyValue>::from_property_value(PropertyValue::Null)
                .unwrap(),
            None
        );
        assert_eq!(round_trip(Some("x".to_string())), Some("x".to_string()));
    }

    #[test]
    fn collections_round_trip() {
        assert_eq!(round_trip(vec![1i64, 2, 3]), vec![1i64, 2, 3]);
        let m: BTreeMap<String, i64> =
            [("a".to_string(), 1i64)].into_iter().collect();
        assert_eq!(round_trip(m.clone()), m);
    }

    #[test]
    fn a_box_is_transparent_on_the_wire() {
        // Generated code boxes a field that would otherwise make its struct
        // infinitely sized. The box is a Rust detail; the wire sees the value.
        let boxed = Box::new("x".to_string());
        assert_eq!(boxed.clone().into_property_value(), PropertyValue::String("x".into()));
        let back: Box<String> =
            FromPropertyValue::from_property_value(PropertyValue::String("x".into())).unwrap();
        assert_eq!(*back, "x".to_string());
    }

    #[test]
    fn an_optional_box_round_trips_both_ways() {
        // Option<Box<T>> is the exact shape a boxed optional field has.
        let some: Option<Box<i64>> = Some(Box::new(7));
        let v = some.into_property_value();
        let back: Option<Box<i64>> = FromPropertyValue::from_property_value(v).unwrap();
        assert_eq!(back.map(|b| *b), Some(7));

        let none: Option<Box<i64>> =
            FromPropertyValue::from_property_value(PropertyValue::Null).unwrap();
        assert!(none.is_none());
    }

    #[test]
    fn conversion_sees_through_a_secret_wrapper() {
        // A secret arriving from the engine must still convert to its type;
        // secretness is tracked on the Output, not the value.
        let v = PropertyValue::Secret(Box::new(PropertyValue::String("s".into())));
        assert_eq!(
            <String as FromPropertyValue>::from_property_value(v).unwrap(),
            "s".to_string()
        );
    }

    #[test]
    fn a_type_mismatch_names_the_expected_type_and_shows_the_value() {
        let err = <i64 as FromPropertyValue>::from_property_value(
            PropertyValue::String("x".into()),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("integer"), "error does not name the expected type: {err}");
        assert!(err.contains("x"), "error does not show the offending value: {err}");
    }

    #[test]
    fn non_utf8_bytes_refuse_to_become_a_rust_string() {
        // Silently lossy-converting would corrupt the value; the error has to
        // say why, since the wire type is still "string".
        let err = <String as FromPropertyValue>::from_property_value(
            PropertyValue::ByteString(vec![0xff]),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("non-UTF8"), "unhelpful error: {err}");
    }
}
