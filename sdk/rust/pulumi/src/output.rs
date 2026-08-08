//! `Output<T>`: asynchronous, possibly-unknown, possibly-secret values.
//!
//! Outputs carry three things alongside their (future) value: whether the
//! value is known yet (during previews it may not be), whether it is secret,
//! and which resources it depends on. Combinators propagate all three.

use std::collections::BTreeMap;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;

use futures::future::{BoxFuture, FutureExt, Shared};

use crate::convert::{FromPropertyValue, IntoPropertyValue};
use crate::value::{OutputValue, PropertyValue};

/// The resolved state of an output: a property value (which is
/// [`PropertyValue::Computed`] when unknown), a secret flag, and the URNs of
/// resources the value depends on.
#[derive(Clone, Debug)]
pub struct OutputData {
    pub value: PropertyValue,
    pub secret: bool,
    pub deps: Vec<String>,
}

impl OutputData {
    pub fn known(&self) -> bool {
        !self.value.contains_unknown()
    }

    /// Normalize a raw property value into output data, lifting any
    /// top-level secret/output wrappers into the flags.
    pub fn from_value(v: PropertyValue) -> OutputData {
        match v {
            PropertyValue::Secret(inner) => {
                let mut d = OutputData::from_value(*inner);
                d.secret = true;
                d
            }
            PropertyValue::Output(OutputValue { value, secret, dependencies }) => {
                let mut d = match value {
                    Some(inner) => OutputData::from_value(*inner),
                    None => OutputData {
                        value: PropertyValue::Computed,
                        secret: false,
                        deps: vec![],
                    },
                };
                d.secret |= secret;
                d.deps.extend(dependencies);
                d
            }
            value => OutputData { value, secret: false, deps: vec![] },
        }
    }

    /// Re-wrap this data as a single property value, encoding secretness,
    /// unknownness, and dependencies as a first-class output value when
    /// needed. Only a bare unknown collapses; collections with unknown
    /// elements stay partially known, with element wrappers inline.
    pub fn into_value(self) -> PropertyValue {
        let top_unknown = matches!(self.value, PropertyValue::Computed);
        if self.deps.is_empty() && !top_unknown {
            if self.secret {
                return PropertyValue::Secret(Box::new(self.value));
            }
            return self.value;
        }
        PropertyValue::Output(OutputValue {
            value: if top_unknown { None } else { Some(Box::new(self.value)) },
            secret: self.secret,
            dependencies: self.deps,
        })
    }
}

type SharedData = Shared<BoxFuture<'static, OutputData>>;

/// An asynchronous value flowing through a Pulumi program.
pub struct Output<T> {
    data: SharedData,
    _t: PhantomData<fn() -> T>,
}

impl<T> Clone for Output<T> {
    fn clone(&self) -> Self {
        Output { data: self.data.clone(), _t: PhantomData }
    }
}

impl<T> std::fmt::Debug for Output<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Output<..>")
    }
}

impl<T> Output<T> {
    /// Build an output from a future resolving to [`OutputData`].
    pub fn from_data_future(fut: impl Future<Output = OutputData> + Send + 'static) -> Self {
        Output { data: fut.boxed().shared(), _t: PhantomData }
    }

    /// Build an output from already-resolved data.
    pub fn from_data(data: OutputData) -> Self {
        Output::from_data_future(std::future::ready(data))
    }

    /// A known, non-secret output holding a raw property value.
    pub fn from_value(v: PropertyValue) -> Self {
        Output::from_data(OutputData::from_value(v))
    }

    /// An unknown output (used during previews).
    pub fn unknown() -> Self {
        Output::from_data(OutputData {
            value: PropertyValue::Computed,
            secret: false,
            deps: vec![],
        })
    }

    /// Await the resolved data of this output.
    pub async fn data(&self) -> OutputData {
        self.data.clone().await
    }

    /// Await this output and re-encode it as a single property value.
    pub async fn into_property_value(self) -> PropertyValue {
        self.data.await.into_value()
    }

    /// Mark this output secret.
    pub fn as_secret(&self) -> Output<T> {
        let data = self.data.clone();
        Output::from_data_future(async move {
            let mut d = data.await;
            d.secret = true;
            d
        })
    }

    /// Reinterpret the element type. The dynamic payload is untouched; this
    /// is a typed-layer cast used by generated code.
    pub fn cast<U>(&self) -> Output<U> {
        Output { data: self.data.clone(), _t: PhantomData }
    }

    /// Index into an object (by key) or array (by position), producing the
    /// element as a dynamic output. Unknowns, secretness, and dependencies
    /// propagate.
    pub fn index(&self, key: impl Into<PropIndex>) -> Output<PropertyValue> {
        let key = key.into();
        let data = self.data.clone();
        Output::from_data_future(async move {
            let d = data.await;
            // Only a wholly-unknown container blocks indexing; containers
            // with unknown elements still navigate.
            if matches!(d.value, PropertyValue::Computed) {
                return d;
            }
            let elem = index_value(&d.value, &key);
            let inner = OutputData::from_value(elem);
            OutputData {
                value: inner.value,
                secret: d.secret || inner.secret,
                deps: d.deps.into_iter().chain(inner.deps).collect(),
            }
        })
    }
}

/// A key for [`Output::index`]: an object key or array index.
#[derive(Clone, Debug)]
pub enum PropIndex {
    Key(String),
    Index(usize),
}

impl From<&str> for PropIndex {
    fn from(s: &str) -> Self {
        PropIndex::Key(s.to_string())
    }
}

impl From<String> for PropIndex {
    fn from(s: String) -> Self {
        PropIndex::Key(s)
    }
}

impl From<usize> for PropIndex {
    fn from(i: usize) -> Self {
        PropIndex::Index(i)
    }
}

fn index_value(v: &PropertyValue, key: &PropIndex) -> PropertyValue {
    // Look through transparent wrappers so indexing works on secrets too.
    match v {
        PropertyValue::Secret(inner) => {
            return PropertyValue::Secret(Box::new(index_value(inner, key)))
        }
        PropertyValue::Output(o) => {
            if let Some(inner) = &o.value {
                let mut out = o.clone();
                out.value = Some(Box::new(index_value(inner, key)));
                return PropertyValue::Output(out);
            }
            return v.clone();
        }
        _ => {}
    }
    match (v, key) {
        (PropertyValue::Object(m), PropIndex::Key(k)) => {
            m.get(k).cloned().unwrap_or(PropertyValue::Null)
        }
        (PropertyValue::Array(a), PropIndex::Index(i)) => {
            a.get(*i).cloned().unwrap_or(PropertyValue::Null)
        }
        (PropertyValue::Array(a), PropIndex::Key(k)) => {
            // PCL allows numeric string keys on arrays.
            match k.parse::<usize>() {
                Ok(i) => a.get(i).cloned().unwrap_or(PropertyValue::Null),
                Err(_) => PropertyValue::Null,
            }
        }
        (PropertyValue::Object(m), PropIndex::Index(i)) => {
            m.get(&i.to_string()).cloned().unwrap_or(PropertyValue::Null)
        }
        _ => PropertyValue::Null,
    }
}

impl<T: IntoPropertyValue> Output<T> {
    /// A known output holding `value`.
    pub fn known(value: T) -> Self {
        Output::from_value(value.into_property_value())
    }

    /// A known secret output holding `value`.
    pub fn secret(value: T) -> Self {
        Output::from_data(OutputData {
            value: value.into_property_value(),
            secret: true,
            deps: vec![],
        })
    }
}

impl<T: FromPropertyValue + Send + 'static> Output<T> {
    /// Transform the value with `f` once it resolves.
    ///
    /// If the value is unknown, `f` does not run and unknownness (plus
    /// secretness and dependencies) propagates.
    pub fn map<U, F>(&self, f: F) -> Output<U>
    where
        U: IntoPropertyValue + Send + 'static,
        F: FnOnce(T) -> U + Send + 'static,
    {
        self.then(move |t| std::future::ready(f(t)))
    }

    /// Like [`Output::map`], but `f` returns a future.
    pub fn then<U, F, Fut>(&self, f: F) -> Output<U>
    where
        U: IntoPropertyValue + Send + 'static,
        F: FnOnce(T) -> Fut + Send + 'static,
        Fut: Future<Output = U> + Send,
    {
        let data = self.data.clone();
        Output::from_data_future(async move {
            let d = data.await;
            if !d.known() {
                return d;
            }
            let t = match T::from_property_value(d.value.clone()) {
                Ok(t) => t,
                Err(e) => panic!("output value conversion failed: {e}"),
            };
            let mapped = f(t).await;
            let inner = OutputData::from_value(mapped.into_property_value());
            OutputData {
                value: inner.value,
                secret: d.secret || inner.secret,
                deps: d.deps.into_iter().chain(inner.deps).collect(),
            }
        })
    }

    /// Like [`Output::then`], but `f` returns another output.
    pub fn flat_map<U, F>(&self, f: F) -> Output<U>
    where
        F: FnOnce(T) -> Output<U> + Send + 'static,
    {
        let data = self.data.clone();
        Output::from_data_future(async move {
            let d = data.await;
            if !d.known() {
                return d;
            }
            let t = match T::from_property_value(d.value.clone()) {
                Ok(t) => t,
                Err(e) => panic!("output value conversion failed: {e}"),
            };
            let inner = f(t).data().await;
            OutputData {
                value: inner.value,
                secret: d.secret || inner.secret,
                deps: d.deps.into_iter().chain(inner.deps).collect(),
            }
        })
    }
}

/// Combine several outputs into one array-valued output. The array itself
/// stays known even when elements are unknown: element-level unknownness,
/// secretness, and dependencies are encoded inline on each element, matching
/// how other Pulumi SDKs support partially-known collections.
pub fn all(outputs: Vec<Output<PropertyValue>>) -> Output<Vec<PropertyValue>> {
    Output::from_data_future(async move {
        let mut values = Vec::with_capacity(outputs.len());
        let mut deps = vec![];
        for o in outputs {
            let d = o.data().await;
            deps.extend(d.deps.clone());
            values.push(d.into_value());
        }
        OutputData { value: PropertyValue::Array(values), secret: false, deps }
    })
}

/// Concatenate string outputs (the engine for interpolated strings in
/// generated programs). Unknown parts make the whole string unknown.
pub fn concat(parts: Vec<Output<PropertyValue>>) -> Output<String> {
    Output::from_data_future(async move {
        let mut s = String::new();
        let mut secret = false;
        let mut deps = vec![];
        let mut known = true;
        for p in parts {
            let d = p.data().await;
            secret |= d.secret;
            if !d.known() {
                known = false;
            } else {
                s.push_str(&display_value(&d.value));
            }
            deps.extend(d.deps);
        }
        OutputData {
            value: if known { PropertyValue::String(s) } else { PropertyValue::Computed },
            secret,
            deps,
        }
    })
}

/// Render a property value the way Pulumi programs interpolate values into
/// strings.
fn display_value(v: &PropertyValue) -> String {
    match v {
        PropertyValue::Null => String::new(),
        PropertyValue::Bool(b) => b.to_string(),
        PropertyValue::Number(n) => {
            if n.fract() == 0.0 && n.abs() < 1e15 {
                format!("{}", *n as i64)
            } else {
                n.to_string()
            }
        }
        PropertyValue::String(s) => s.clone(),
        other => format!("{other:?}"),
    }
}

impl<T: IntoPropertyValue> From<T> for Output<T> {
    fn from(v: T) -> Self {
        Output::known(v)
    }
}

/// Convert typed inputs into the dynamic output form used to marshal
/// resource inputs.
pub fn to_dynamic<T>(o: &Output<T>) -> Output<PropertyValue> {
    o.cast()
}

/// Build an object-valued output from named fields, preserving each field's
/// unknownness/secretness inline (fields become first-class output values in
/// the object when they carry deps or unknowns).
pub fn object(fields: Vec<(String, Output<PropertyValue>)>) -> Output<PropertyValue> {
    Output::from_data_future(async move {
        let mut m = BTreeMap::new();
        let mut deps = vec![];
        for (k, o) in fields {
            let d = o.data().await;
            deps.extend(d.deps.clone());
            m.insert(k, d.into_value());
        }
        OutputData { value: PropertyValue::Object(m), secret: false, deps }
    })
}

/// A pinned boxed future, the shape SDK async helpers return.
pub type PulumiFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn map_known() {
        let o = Output::known(21i64).map(|v| v * 2);
        let d = o.data().await;
        assert_eq!(d.value, PropertyValue::Number(42.0));
        assert!(!d.secret);
    }

    #[tokio::test]
    async fn map_propagates_secret_and_unknown() {
        let o: Output<i64> = Output::from_data(OutputData {
            value: PropertyValue::Computed,
            secret: true,
            deps: vec!["urn:x".into()],
        });
        let mapped = o.map(|v| v + 1);
        let d = mapped.data().await;
        assert!(!d.known());
        assert!(d.secret);
        assert_eq!(d.deps, vec!["urn:x".to_string()]);
    }

    #[tokio::test]
    async fn concat_strings() {
        let parts = vec![
            Output::from_value(PropertyValue::String("n=".into())),
            Output::from_value(PropertyValue::Number(3.0)),
        ];
        let d = concat(parts).data().await;
        assert_eq!(d.value, PropertyValue::String("n=3".into()));
    }
}
