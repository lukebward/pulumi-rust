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
            PropertyValue::Output(OutputValue {
                value,
                secret,
                dependencies,
            }) => {
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
            value => OutputData {
                value,
                secret: false,
                deps: vec![],
            },
        }
    }

    /// Re-wrap this data as a single property value, encoding secretness,
    /// unknownness, and dependencies as a first-class output value when
    /// needed. Only a bare unknown collapses; collections with unknown
    /// elements stay partially known, with element wrappers inline.
    pub fn into_value(self) -> PropertyValue {
        // A failed value is unknown, like a computed one: the output value
        // must carry no value at all, or the engine reads the unknown
        // sentinel as an ordinary string.
        let top_unknown = matches!(
            self.value,
            PropertyValue::Computed | PropertyValue::Failed(_)
        );
        if self.deps.is_empty() && !top_unknown {
            if self.secret {
                return PropertyValue::Secret(Box::new(self.value));
            }
            return self.value;
        }
        PropertyValue::Output(OutputValue {
            value: if top_unknown {
                None
            } else {
                Some(Box::new(self.value))
            },
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
        Output {
            data: self.data.clone(),
            _t: PhantomData,
        }
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
        Output {
            data: fut.boxed().shared(),
            _t: PhantomData,
        }
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
        Output {
            data: self.data.clone(),
            _t: PhantomData,
        }
    }

    /// Index into an object (by key) or array (by position), producing the
    /// element as a dynamic output. Unknowns, secretness, and dependencies
    /// propagate.
    /// Index, reporting an absent key as the missing sentinel rather than
    /// null. Generated code uses this only inside `try`/`can`, so ordinary
    /// lookups keep null semantics.
    pub fn index_checked(&self, key: impl Into<PropIndex>) -> Output<PropertyValue> {
        let key = key.into();
        let data = self.data.clone();
        Output::from_data_future(async move {
            let d = data.await;
            if matches!(d.value, PropertyValue::Computed) {
                return d;
            }
            let elem = index_value_checked(&d.value, &key);
            let inner = OutputData::from_value(elem);
            OutputData {
                value: inner.value,
                secret: d.secret || inner.secret,
                deps: d.deps.into_iter().chain(inner.deps).collect(),
            }
        })
    }

    /// Ensure a resource-reference value's resource is hydrated, keeping the
    /// value itself unchanged. Generated accessors for resource-typed
    /// outputs use this so the engine hydrates the reference even when the
    /// program only forwards it.
    pub fn hydrated(&self) -> Output<T> {
        let data = self.data.clone();
        Output::from_data_future(async move {
            let d = data.await;
            if matches!(d.value, PropertyValue::Computed) {
                return d;
            }
            OutputData {
                value: crate::context::touch_reference(d.value).await,
                secret: d.secret,
                deps: d.deps,
            }
        })
    }

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
            // Reading a property off a resource reference means fetching
            // that resource's state from the engine first.
            let value = crate::context::hydrate(d.value).await;
            let elem = index_value(&value, &key);
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

/// Like [`index_value`] but reports an absent key as
/// [`PropertyValue::Missing`].
fn index_value_checked(v: &PropertyValue, key: &PropIndex) -> PropertyValue {
    match v {
        PropertyValue::Secret(inner) => {
            return PropertyValue::Secret(Box::new(index_value_checked(inner, key)))
        }
        PropertyValue::Output(o) => {
            if let Some(inner) = &o.value {
                let mut out = o.clone();
                out.value = Some(Box::new(index_value_checked(inner, key)));
                return PropertyValue::Output(out);
            }
            return v.clone();
        }
        _ => {}
    }
    let present = match (v, key) {
        (PropertyValue::Object(m), PropIndex::Key(k)) => m.contains_key(k),
        (PropertyValue::Array(a), PropIndex::Index(i)) => *i < a.len(),
        (PropertyValue::Array(a), PropIndex::Key(k)) => {
            k.parse::<usize>().map(|i| i < a.len()).unwrap_or(false)
        }
        _ => false,
    };
    if !present {
        return PropertyValue::Missing;
    }
    index_value(v, key)
}

/// PCL indexing semantics, shared with [`crate::ops::index`] so the two
/// entry points cannot drift: `ops` had a line-for-line copy of this,
/// wrapper look-through and numeric-string-key rule included.
pub(crate) fn index_value(v: &PropertyValue, key: &PropIndex) -> PropertyValue {
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
        (PropertyValue::Object(m), PropIndex::Index(i)) => m
            .get(&i.to_string())
            .cloned()
            .unwrap_or(PropertyValue::Null),
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

/// The result a combinator yields when its input is not fully known.
///
/// Short-circuiting must not hand the *source* data back: the combinator's
/// result has a different type and shape, so returning the input meant
/// `join(", ", [res.name, "x"])` previewed as the two-element array rather
/// than an unknown string, and the engine diffed a list against a string.
/// Secretness and dependencies still ride along. A top-level `Failed` is
/// kept intact, because `recover`/`try`/`can` read the failure message off
/// the value itself; collapsing it would silently disarm them.
fn short_circuit(d: OutputData) -> OutputData {
    let OutputData {
        value,
        secret,
        deps,
    } = d;
    let value = match value {
        failed @ PropertyValue::Failed(_) => failed,
        _ => PropertyValue::Computed,
    };
    OutputData {
        value,
        secret,
        deps,
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
                return short_circuit(d);
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
                return short_circuit(d);
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
        OutputData {
            value: PropertyValue::Array(values),
            secret: false,
            deps,
        }
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
            value: if known {
                PropertyValue::String(s)
            } else {
                PropertyValue::Computed
            },
            secret,
            deps,
        }
    })
}

/// Render a property value the way Pulumi programs interpolate values into
/// strings.
/// Render a value as a string the way string interpolation does.
pub(crate) fn display(v: &PropertyValue) -> String {
    display_value(v)
}

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
        // Interpolating a non-UTF8 value is inherently lossy, but a debug
        // dump of the raw bytes would be worse.
        PropertyValue::ByteString(b) => String::from_utf8_lossy(b).into_owned(),
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
        OutputData {
            value: PropertyValue::Object(m),
            secret: false,
            deps,
        }
    })
}

/// A pinned boxed future, the shape SDK async helpers return.
pub type PulumiFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Resolves a [`deferred_output`] once the producing value is available.
pub struct DeferredResolver {
    tx: tokio::sync::oneshot::Sender<Output<PropertyValue>>,
}

impl DeferredResolver {
    /// Supply the value the deferred output stands for.
    pub fn resolve(self, value: Output<PropertyValue>) {
        let _ = self.tx.send(value);
    }
}

/// An output whose value arrives later, used to break the cycle between two
/// components that each consume the other's outputs. An unresolved deferred
/// output reads as unknown rather than hanging.
pub fn deferred_output() -> (Output<PropertyValue>, DeferredResolver) {
    let (tx, rx) = tokio::sync::oneshot::channel::<Output<PropertyValue>>();
    let out = Output::from_data_future(async move {
        match rx.await {
            Ok(o) => o.data().await,
            Err(_) => OutputData {
                value: PropertyValue::Computed,
                secret: false,
                deps: vec![],
            },
        }
    });
    (out, DeferredResolver { tx })
}

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

    // --- the three things every combinator must propagate -------------------
    //
    // Unknown short-circuits, secretness is sticky, dependencies union. Every
    // Pulumi SDK guarantees all three; these pin them per combinator so a
    // refactor cannot quietly drop one.

    fn data(value: PropertyValue, secret: bool, deps: &[&str]) -> OutputData {
        OutputData {
            value,
            secret,
            deps: deps.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[tokio::test]
    async fn map_does_not_run_on_unknown() {
        let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = ran.clone();
        let o: Output<i64> = Output::unknown();
        let mapped = o.map(move |v| {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            v
        });
        assert!(!mapped.data().await.known());
        assert!(
            !ran.load(std::sync::atomic::Ordering::SeqCst),
            "map ran its closure over an unknown value"
        );
    }

    #[tokio::test]
    async fn map_keeps_secret_of_a_known_value() {
        let o: Output<i64> = Output::from_data(data(PropertyValue::Number(1.0), true, &[]));
        assert!(
            o.map(|v| v + 1).data().await.secret,
            "secretness is not sticky"
        );
    }

    #[tokio::test]
    async fn map_unions_deps_from_both_sides() {
        // The source depends on A; the mapped-to value carries a dependency on
        // B as an inline output value. The result must depend on both.
        let o: Output<i64> = Output::from_data(data(PropertyValue::Number(1.0), false, &["urn:a"]));
        let mapped = o.map(|_| {
            PropertyValue::Output(OutputValue {
                value: Some(Box::new(PropertyValue::Number(2.0))),
                secret: false,
                dependencies: vec!["urn:b".into()],
            })
        });
        let d = mapped.data().await;
        assert_eq!(d.deps, vec!["urn:a".to_string(), "urn:b".to_string()]);
    }

    #[tokio::test]
    async fn map_takes_secret_from_the_produced_value() {
        let o: Output<i64> = Output::known(1i64);
        let mapped = o.map(|_| PropertyValue::Secret(Box::new(PropertyValue::Number(2.0))));
        assert!(mapped.data().await.secret);
    }

    #[tokio::test]
    async fn flat_map_unions_secret_and_deps() {
        let o: Output<i64> = Output::from_data(data(PropertyValue::Number(1.0), false, &["urn:a"]));
        let out = o.flat_map(|_| -> Output<PropertyValue> {
            Output::from_data(data(PropertyValue::Number(2.0), true, &["urn:b"]))
        });
        let d = out.data().await;
        assert!(d.secret);
        assert_eq!(d.deps, vec!["urn:a".to_string(), "urn:b".to_string()]);
    }

    #[tokio::test]
    async fn flat_map_does_not_run_on_unknown() {
        let o: Output<i64> = Output::unknown();
        let out = o.flat_map(|_| -> Output<PropertyValue> {
            panic!("flat_map ran its closure over an unknown value")
        });
        assert!(!out.data().await.known());
    }

    #[tokio::test]
    async fn map_over_a_deep_unknown_yields_an_unknown_not_the_source_value() {
        // Regression: the short-circuit handed the *source* data back, so the
        // result carried the input's value under the output's type. During a
        // preview `join(", ", [res.name, "x"])` produced the two-element array
        // where an unknown string belonged, and the engine diffed a list
        // against a string.
        let src: Output<Vec<PropertyValue>> = Output::from_data(data(
            PropertyValue::Array(vec![
                PropertyValue::String("x".into()),
                PropertyValue::Computed,
            ]),
            true,
            &["urn:a"],
        ));
        let d = src
            .map(|_| PropertyValue::String("joined".into()))
            .data()
            .await;
        assert_eq!(
            d.value,
            PropertyValue::Computed,
            "the source array leaked through"
        );
        assert!(d.secret, "the short-circuit dropped secretness");
        assert_eq!(d.deps, vec!["urn:a".to_string()]);
    }

    #[tokio::test]
    async fn flat_map_over_a_deep_unknown_yields_an_unknown() {
        let src: Output<Vec<PropertyValue>> = Output::from_data(data(
            PropertyValue::Array(vec![PropertyValue::Computed]),
            false,
            &[],
        ));
        let d = src
            .flat_map(|_| -> Output<PropertyValue> {
                panic!("flat_map ran its closure over an unknown value")
            })
            .data()
            .await;
        assert_eq!(d.value, PropertyValue::Computed);
    }

    #[tokio::test]
    async fn joining_a_partially_unknown_list_is_an_unknown_string() {
        // The end-to-end shape the short-circuit fix is for: `all` keeps the
        // array known with the unknown encoded inline, and the `map` inside
        // `join` must turn that into an unknown string.
        let joined = crate::pv::join(
            crate::pv::string(", "),
            all(vec![
                Output::unknown(),
                Output::from_value(PropertyValue::String("x".into())),
            ])
            .cast(),
        );
        let d = joined.data().await;
        assert_eq!(
            d.value,
            PropertyValue::Computed,
            "join leaked its argument list"
        );
    }

    #[tokio::test]
    async fn map_keeps_a_failed_value_intact() {
        // `Failed` reads as unknown, but it is also the marker `recover`,
        // `try` and `can` pull the failure message off. Collapsing it to a
        // bare unknown on the short-circuit would silently disarm them.
        let src: Output<PropertyValue> =
            Output::from_data(data(PropertyValue::Failed("boom".into()), false, &[]));
        let d = src.map(|v: PropertyValue| v).data().await;
        assert_eq!(d.value, PropertyValue::Failed("boom".into()));
    }

    #[tokio::test]
    async fn as_secret_marks_a_known_value() {
        let d = Output::known(1i64).as_secret().data().await;
        assert!(d.secret);
        assert_eq!(d.value, PropertyValue::Number(1.0));
    }

    // --- OutputData <-> PropertyValue -------------------------------------

    #[test]
    fn into_value_wraps_a_bare_secret() {
        let v = data(PropertyValue::String("s".into()), true, &[]).into_value();
        assert_eq!(
            v,
            PropertyValue::Secret(Box::new(PropertyValue::String("s".into())))
        );
    }

    #[test]
    fn into_value_leaves_a_plain_known_value_alone() {
        let v = data(PropertyValue::String("s".into()), false, &[]).into_value();
        assert_eq!(v, PropertyValue::String("s".into()));
    }

    #[test]
    fn into_value_carries_deps_as_an_output_value() {
        match data(PropertyValue::Number(1.0), false, &["urn:a"]).into_value() {
            PropertyValue::Output(o) => {
                assert_eq!(o.value.as_deref(), Some(&PropertyValue::Number(1.0)));
                assert_eq!(o.dependencies, vec!["urn:a".to_string()]);
            }
            other => panic!("expected an output value, got {other:?}"),
        }
    }

    #[test]
    fn into_value_drops_the_value_when_unknown() {
        // An output value carrying deps must have no value at all when
        // unknown, or the engine reads the unknown sentinel as a real string.
        match data(PropertyValue::Computed, false, &["urn:a"]).into_value() {
            PropertyValue::Output(o) => assert!(o.value.is_none()),
            other => panic!("expected an output value, got {other:?}"),
        }
    }

    #[test]
    fn into_value_treats_failed_as_unknown() {
        // Regression: `Failed` is unknown just like `Computed`. Treating it as
        // known let a failed value marshal as the literal unknown sentinel
        // string, which the engine then took for real data.
        let failed = PropertyValue::Failed("boom".into());
        match data(failed, false, &["urn:a"]).into_value() {
            PropertyValue::Output(o) => assert!(o.value.is_none()),
            other => panic!("expected an output value, got {other:?}"),
        }
    }

    #[test]
    fn from_value_lifts_a_nested_secret_output() {
        let v = PropertyValue::Output(OutputValue {
            value: Some(Box::new(PropertyValue::Secret(Box::new(
                PropertyValue::Number(1.0),
            )))),
            secret: false,
            dependencies: vec!["urn:a".into()],
        });
        let d = OutputData::from_value(v);
        assert_eq!(d.value, PropertyValue::Number(1.0));
        assert!(d.secret, "an inner secret must lift to the flag");
        assert_eq!(d.deps, vec!["urn:a".to_string()]);
    }

    #[test]
    fn from_value_of_a_valueless_output_is_unknown() {
        let v = PropertyValue::Output(OutputValue {
            value: None,
            secret: false,
            dependencies: vec![],
        });
        assert!(!OutputData::from_value(v).known());
    }

    #[test]
    fn known_sees_through_containers() {
        let v = PropertyValue::Array(vec![PropertyValue::Number(1.0), PropertyValue::Computed]);
        assert!(!data(v, false, &[]).known());
    }

    // --- all / concat ------------------------------------------------------

    #[tokio::test]
    async fn all_keeps_the_array_known_when_an_element_is_not() {
        // Partially-known collections are a real Pulumi behaviour: the array
        // stays known and the unknown element is encoded inline.
        let d = all(vec![
            Output::from_value(PropertyValue::Number(1.0)),
            Output::unknown(),
        ])
        .data()
        .await;
        match &d.value {
            PropertyValue::Array(vs) => {
                assert_eq!(vs.len(), 2);
                assert_eq!(vs[0], PropertyValue::Number(1.0));
                assert!(vs[1].contains_unknown());
            }
            other => panic!("expected an array, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn all_unions_element_deps() {
        let d = all(vec![
            Output::from_data(data(PropertyValue::Number(1.0), false, &["urn:a"])),
            Output::from_data(data(PropertyValue::Number(2.0), false, &["urn:b"])),
        ])
        .data()
        .await;
        assert_eq!(d.deps, vec!["urn:a".to_string(), "urn:b".to_string()]);
    }

    #[tokio::test]
    async fn concat_is_unknown_if_any_part_is() {
        let d = concat(vec![
            Output::from_value(PropertyValue::String("a".into())),
            Output::unknown(),
        ])
        .data()
        .await;
        assert!(
            !d.known(),
            "one unknown part must make the whole string unknown"
        );
    }

    #[tokio::test]
    async fn concat_is_secret_if_any_part_is() {
        let d = concat(vec![
            Output::from_value(PropertyValue::String("a".into())),
            Output::from_data(data(PropertyValue::String("b".into()), true, &[])),
        ])
        .data()
        .await;
        assert!(d.secret);
    }

    #[tokio::test]
    async fn concat_unions_deps() {
        let d = concat(vec![
            Output::from_data(data(PropertyValue::String("a".into()), false, &["urn:a"])),
            Output::from_data(data(PropertyValue::String("b".into()), false, &["urn:b"])),
        ])
        .data()
        .await;
        assert_eq!(d.deps, vec!["urn:a".to_string(), "urn:b".to_string()]);
    }

    // --- indexing ----------------------------------------------------------

    fn obj(pairs: &[(&str, PropertyValue)]) -> PropertyValue {
        PropertyValue::Object(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        )
    }

    #[tokio::test]
    async fn index_reads_object_keys_and_array_positions() {
        let o = Output::<PropertyValue>::from_value(obj(&[(
            "xs",
            PropertyValue::Array(vec![PropertyValue::Number(7.0)]),
        )]));
        let d = o.index("xs").index(0usize).data().await;
        assert_eq!(d.value, PropertyValue::Number(7.0));
    }

    #[tokio::test]
    async fn index_accepts_a_numeric_string_key_on_an_array() {
        // PCL indexes arrays with string keys; the SDK has to accept both.
        let o =
            Output::<PropertyValue>::from_value(PropertyValue::Array(vec![PropertyValue::Number(
                7.0,
            )]));
        assert_eq!(o.index("0").data().await.value, PropertyValue::Number(7.0));
    }

    #[tokio::test]
    async fn index_of_an_absent_key_is_null() {
        let o = Output::<PropertyValue>::from_value(obj(&[]));
        assert_eq!(o.index("nope").data().await.value, PropertyValue::Null);
    }

    #[tokio::test]
    async fn index_checked_of_an_absent_key_is_missing() {
        // Regression: the missing sentinel exists so `try`/`can` can tell an
        // absent key from a null one. It must stay inside index_checked —
        // leaking it into ordinary indexing broke `== null` comparisons.
        let o = Output::<PropertyValue>::from_value(obj(&[]));
        assert_eq!(
            o.index_checked("nope").data().await.value,
            PropertyValue::Missing
        );
    }

    #[tokio::test]
    async fn index_of_an_unknown_container_stays_unknown() {
        let o = Output::<PropertyValue>::unknown();
        assert!(!o.index("k").data().await.known());
    }

    #[tokio::test]
    async fn index_lifts_an_element_secret() {
        let o = Output::<PropertyValue>::from_value(obj(&[(
            "k",
            PropertyValue::Secret(Box::new(PropertyValue::Number(1.0))),
        )]));
        let d = o.index("k").data().await;
        assert!(
            d.secret,
            "a secret element must make the indexed output secret"
        );
        assert_eq!(d.value, PropertyValue::Number(1.0));
    }

    #[tokio::test]
    async fn index_keeps_container_deps() {
        let o = Output::<PropertyValue>::from_data(data(
            obj(&[("k", PropertyValue::Number(1.0))]),
            false,
            &["urn:a"],
        ));
        assert_eq!(o.index("k").data().await.deps, vec!["urn:a".to_string()]);
    }

    // --- object / deferred -------------------------------------------------

    #[tokio::test]
    async fn object_builds_a_map_and_unions_deps() {
        let d = object(vec![
            (
                "a".to_string(),
                Output::from_data(data(PropertyValue::Number(1.0), false, &["urn:a"])),
            ),
            (
                "b".to_string(),
                Output::from_value(PropertyValue::Number(2.0)),
            ),
        ])
        .data()
        .await;
        match &d.value {
            PropertyValue::Object(m) => {
                assert_eq!(m.len(), 2);
                assert_eq!(m.get("b"), Some(&PropertyValue::Number(2.0)));
            }
            other => panic!("expected an object, got {other:?}"),
        }
        assert_eq!(d.deps, vec!["urn:a".to_string()]);
    }

    #[tokio::test]
    async fn an_unresolved_deferred_output_reads_as_unknown() {
        // Dropping the resolver must not hang the program: a component whose
        // deferred input never arrives previews as unknown.
        let (out, resolver) = deferred_output();
        drop(resolver);
        assert!(!out.data().await.known());
    }

    #[tokio::test]
    async fn a_resolved_deferred_output_carries_the_value_through() {
        let (out, resolver) = deferred_output();
        resolver.resolve(Output::from_data(data(
            PropertyValue::Number(9.0),
            true,
            &["urn:a"],
        )));
        let d = out.data().await;
        assert_eq!(d.value, PropertyValue::Number(9.0));
        assert!(d.secret);
        assert_eq!(d.deps, vec!["urn:a".to_string()]);
    }
}
