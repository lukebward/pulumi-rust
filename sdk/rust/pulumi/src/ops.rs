//! Dynamic operators over output values, used by generated programs for PCL
//! binary/unary/conditional expressions. Unknownness, secretness, and
//! dependencies propagate through every operator.

use crate::output::{Output, OutputData};
use crate::value::PropertyValue;

fn combine2(
    a: Output<PropertyValue>,
    b: Output<PropertyValue>,
    f: impl FnOnce(PropertyValue, PropertyValue) -> PropertyValue + Send + 'static,
) -> Output<PropertyValue> {
    Output::from_data_future(async move {
        let da = a.data().await;
        let db = b.data().await;
        let secret = da.secret || db.secret;
        let deps: Vec<String> = da.deps.iter().chain(db.deps.iter()).cloned().collect();
        if !da.known() || !db.known() {
            return OutputData {
                value: PropertyValue::Computed,
                secret,
                deps,
            };
        }
        // Lift any wrappers the combination produced (e.g. indexing into an
        // array with secret elements) into the flags.
        let inner = OutputData::from_value(f(da.value, db.value));
        OutputData {
            value: inner.value,
            secret: secret || inner.secret,
            deps: deps.into_iter().chain(inner.deps).collect(),
        }
    })
}

fn as_number(v: &PropertyValue) -> f64 {
    match v {
        PropertyValue::Number(n) => *n,
        PropertyValue::Bool(true) => 1.0,
        PropertyValue::String(s) => s.parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn as_bool(v: &PropertyValue) -> bool {
    match v {
        PropertyValue::Bool(b) => *b,
        PropertyValue::Null => false,
        PropertyValue::Number(n) => *n != 0.0,
        PropertyValue::String(s) => s == "true",
        _ => true,
    }
}

macro_rules! numeric_op {
    ($name:ident, $op:tt) => {
        pub fn $name(a: Output<PropertyValue>, b: Output<PropertyValue>) -> Output<PropertyValue> {
            combine2(a, b, |a, b| PropertyValue::Number(as_number(&a) $op as_number(&b)))
        }
    };
}

numeric_op!(add, +);
numeric_op!(sub, -);
numeric_op!(mul, *);
numeric_op!(div, /);

pub fn rem(a: Output<PropertyValue>, b: Output<PropertyValue>) -> Output<PropertyValue> {
    combine2(a, b, |a, b| {
        PropertyValue::Number(as_number(&a) % as_number(&b))
    })
}

pub fn eq(a: Output<PropertyValue>, b: Output<PropertyValue>) -> Output<PropertyValue> {
    combine2(a, b, |a, b| PropertyValue::Bool(a == b))
}

pub fn neq(a: Output<PropertyValue>, b: Output<PropertyValue>) -> Output<PropertyValue> {
    combine2(a, b, |a, b| PropertyValue::Bool(a != b))
}

macro_rules! compare_op {
    ($name:ident, $op:tt) => {
        pub fn $name(a: Output<PropertyValue>, b: Output<PropertyValue>) -> Output<PropertyValue> {
            combine2(a, b, |a, b| PropertyValue::Bool(as_number(&a) $op as_number(&b)))
        }
    };
}

compare_op!(lt, <);
compare_op!(lte, <=);
compare_op!(gt, >);
compare_op!(gte, >=);

pub fn and(a: Output<PropertyValue>, b: Output<PropertyValue>) -> Output<PropertyValue> {
    combine2(a, b, |a, b| PropertyValue::Bool(as_bool(&a) && as_bool(&b)))
}

pub fn or(a: Output<PropertyValue>, b: Output<PropertyValue>) -> Output<PropertyValue> {
    combine2(a, b, |a, b| PropertyValue::Bool(as_bool(&a) || as_bool(&b)))
}

pub fn not(a: Output<PropertyValue>) -> Output<PropertyValue> {
    a.map(|v: PropertyValue| PropertyValue::Bool(!as_bool(&v)))
        .cast()
}

pub fn neg(a: Output<PropertyValue>) -> Output<PropertyValue> {
    a.map(|v: PropertyValue| PropertyValue::Number(-as_number(&v)))
        .cast()
}

/// A conditional expression. Both branches are evaluated (they are pure
/// values in generated code); the condition picks one.
pub fn cond(
    c: Output<PropertyValue>,
    t: Output<PropertyValue>,
    f: Output<PropertyValue>,
) -> Output<PropertyValue> {
    Output::from_data_future(async move {
        let dc = c.data().await;
        if !dc.known() {
            return OutputData {
                value: PropertyValue::Computed,
                secret: dc.secret,
                deps: dc.deps,
            };
        }
        let branch = if as_bool(&dc.value) { t } else { f };
        let db = branch.data().await;
        OutputData {
            value: db.value,
            secret: dc.secret || db.secret,
            deps: dc.deps.into_iter().chain(db.deps).collect(),
        }
    })
}

fn convert1(
    a: Output<PropertyValue>,
    f: impl Fn(PropertyValue) -> PropertyValue + Send + Sync + 'static,
) -> Output<PropertyValue> {
    Output::from_data_future(async move {
        let d = a.data().await;
        if !d.known() {
            return d;
        }
        let inner = OutputData::from_value(f(d.value));
        OutputData {
            value: inner.value,
            secret: d.secret || inner.secret,
            deps: d.deps.into_iter().chain(inner.deps).collect(),
        }
    })
}

/// Coerce a value to a number (PCL conversion semantics).
pub fn to_number(a: Output<PropertyValue>) -> Output<PropertyValue> {
    convert1(a, |v| match v {
        PropertyValue::Number(n) => PropertyValue::Number(n),
        PropertyValue::String(s) => match s.parse::<f64>() {
            Ok(n) => PropertyValue::Number(n),
            Err(_) => PropertyValue::String(s),
        },
        PropertyValue::Bool(b) => PropertyValue::Number(if b { 1.0 } else { 0.0 }),
        other => other,
    })
}

/// Coerce a value to an integer.
pub fn to_int(a: Output<PropertyValue>) -> Output<PropertyValue> {
    convert1(a, |v| match v {
        PropertyValue::Number(n) => PropertyValue::Number(n.trunc()),
        PropertyValue::String(s) => match s.parse::<f64>() {
            Ok(n) => PropertyValue::Number(n.trunc()),
            Err(_) => PropertyValue::String(s),
        },
        other => other,
    })
}

/// Coerce a value to a bool.
pub fn to_bool(a: Output<PropertyValue>) -> Output<PropertyValue> {
    convert1(a, |v| match v {
        PropertyValue::Bool(b) => PropertyValue::Bool(b),
        PropertyValue::String(s) => match s.as_str() {
            "true" => PropertyValue::Bool(true),
            "false" => PropertyValue::Bool(false),
            _ => PropertyValue::String(s),
        },
        other => other,
    })
}

/// Coerce a value to a string.
pub fn to_string(a: Output<PropertyValue>) -> Output<PropertyValue> {
    convert1(a, |v| match v {
        PropertyValue::String(s) => PropertyValue::String(s),
        // Byte strings pass through untouched; coercing would corrupt them.
        PropertyValue::ByteString(b) => PropertyValue::ByteString(b),
        PropertyValue::Number(n) => {
            if n.fract() == 0.0 && n.abs() < 1e15 {
                PropertyValue::String(format!("{}", n as i64))
            } else {
                PropertyValue::String(n.to_string())
            }
        }
        PropertyValue::Bool(b) => PropertyValue::String(b.to_string()),
        other => other,
    })
}

/// Entries of a collection for `for`-expression evaluation: (key, value)
/// output pairs. Arrays yield numeric keys; objects yield their keys.
fn collection_entries(v: &PropertyValue) -> Vec<(PropertyValue, PropertyValue)> {
    match v {
        PropertyValue::Array(a) => a
            .iter()
            .enumerate()
            .map(|(i, e)| (PropertyValue::Number(i as f64), e.clone()))
            .collect(),
        PropertyValue::Object(m) => m
            .iter()
            .map(|(k, e)| (PropertyValue::String(k.clone()), e.clone()))
            .collect(),
        _ => vec![],
    }
}

/// Evaluate a PCL `for` expression producing a list: `[for k, v in coll :
/// value(k, v) if cond(k, v)]`.
pub fn for_array(
    coll: Output<PropertyValue>,
    cond: impl Fn(Output<PropertyValue>, Output<PropertyValue>) -> Output<PropertyValue>
        + Send
        + 'static,
    value: impl Fn(Output<PropertyValue>, Output<PropertyValue>) -> Output<PropertyValue>
        + Send
        + 'static,
) -> Output<PropertyValue> {
    Output::from_data_future(async move {
        let dc = coll.data().await;
        if matches!(dc.value, PropertyValue::Computed) {
            return dc;
        }
        let mut items = vec![];
        let mut deps = dc.deps.clone();
        let mut secret = dc.secret;
        for (k, v) in collection_entries(&dc.value) {
            let k = Output::from_value(k);
            let v = Output::from_value(v);
            let keep = cond(k.clone(), v.clone()).data().await;
            deps.extend(keep.deps.clone());
            // An unknown condition makes the whole comprehension unknown.
            // Treating it as false silently dropped the element, so
            // `[for v in xs : v if v.enabled]` over a pending resource
            // output previewed as `[]` and the engine reported every
            // element as a deletion.
            if !keep.known() {
                secret |= keep.secret;
                return OutputData {
                    value: PropertyValue::Computed,
                    secret,
                    deps,
                };
            }
            if !matches!(keep.value, PropertyValue::Bool(true)) {
                continue;
            }
            let dv = value(k, v).data().await;
            deps.extend(dv.deps.clone());
            items.push(dv.into_value());
        }
        OutputData {
            value: PropertyValue::Array(items),
            secret,
            deps,
        }
    })
}

/// Evaluate a PCL `for` expression producing an object: `{for k, v in coll :
/// key(k, v) => value(k, v) if cond(k, v)}`.
pub fn for_object(
    coll: Output<PropertyValue>,
    cond: impl Fn(Output<PropertyValue>, Output<PropertyValue>) -> Output<PropertyValue>
        + Send
        + 'static,
    key: impl Fn(Output<PropertyValue>, Output<PropertyValue>) -> Output<PropertyValue> + Send + 'static,
    value: impl Fn(Output<PropertyValue>, Output<PropertyValue>) -> Output<PropertyValue>
        + Send
        + 'static,
) -> Output<PropertyValue> {
    Output::from_data_future(async move {
        let dc = coll.data().await;
        if matches!(dc.value, PropertyValue::Computed) {
            return dc;
        }
        let mut map = std::collections::BTreeMap::new();
        let mut deps = dc.deps.clone();
        let mut secret = dc.secret;
        for (k, v) in collection_entries(&dc.value) {
            let k = Output::from_value(k);
            let v = Output::from_value(v);
            let keep = cond(k.clone(), v.clone()).data().await;
            deps.extend(keep.deps.clone());
            // As in `for_array`: an unknown condition means we do not know
            // which entries survive, so the whole object is unknown rather
            // than silently missing its filtered entries.
            if !keep.known() {
                secret |= keep.secret;
                return OutputData {
                    value: PropertyValue::Computed,
                    secret,
                    deps,
                };
            }
            if !matches!(keep.value, PropertyValue::Bool(true)) {
                continue;
            }
            let dk = key(k.clone(), v.clone()).data().await;
            deps.extend(dk.deps.clone());
            let dv = value(k, v).data().await;
            deps.extend(dv.deps.clone());
            if let PropertyValue::String(ks) = dk.value {
                map.insert(ks, dv.into_value());
            }
        }
        OutputData {
            value: PropertyValue::Object(map),
            secret,
            deps,
        }
    })
}

/// Index with a dynamic key. A container with unknown elements can still be
/// indexed; only a wholly-unknown container (or key) is opaque.
pub fn index(target: Output<PropertyValue>, key: Output<PropertyValue>) -> Output<PropertyValue> {
    Output::from_data_future(async move {
        let dt = target.data().await;
        let dk = key.data().await;
        let secret = dt.secret || dk.secret;
        let deps: Vec<String> = dt.deps.iter().chain(dk.deps.iter()).cloned().collect();
        if matches!(dt.value, PropertyValue::Computed) || !dk.known() {
            return OutputData {
                value: PropertyValue::Computed,
                secret,
                deps,
            };
        }
        let idx = match &dk.value {
            PropertyValue::Number(n) => crate::output::PropIndex::Index(*n as usize),
            PropertyValue::String(s) => crate::output::PropIndex::Key(s.clone()),
            _ => {
                return OutputData {
                    value: PropertyValue::Null,
                    secret,
                    deps,
                };
            }
        };
        // Indexing semantics live in one place: this used to be a
        // line-for-line copy of `output::index_value`, and two copies of the
        // wrapper look-through and numeric-string-key rules had to be kept
        // in step by hand.
        let inner = OutputData::from_value(crate::output::index_value(&dt.value, &idx));
        OutputData {
            value: inner.value,
            secret: secret || inner.secret,
            deps: deps.into_iter().chain(inner.deps).collect(),
        }
    })
}

/// Index into a collection, reporting an absent key as the missing
/// sentinel. Generated code uses this only inside `try`/`can`.
pub fn index_checked(
    target: Output<PropertyValue>,
    key: Output<PropertyValue>,
) -> Output<PropertyValue> {
    Output::from_data_future(async move {
        let dt = target.data().await;
        let dk = key.data().await;
        let secret = dt.secret || dk.secret;
        let deps: Vec<String> = dt.deps.iter().chain(dk.deps.iter()).cloned().collect();
        if matches!(dt.value, PropertyValue::Computed) || !dk.known() {
            return OutputData {
                value: PropertyValue::Computed,
                secret,
                deps,
            };
        }
        let idx = match &dk.value {
            PropertyValue::Number(n) => crate::output::PropIndex::Index(*n as usize),
            PropertyValue::String(s) => crate::output::PropIndex::Key(s.clone()),
            _ => {
                return OutputData {
                    value: PropertyValue::Missing,
                    secret,
                    deps,
                }
            }
        };
        let inner = Output::<PropertyValue>::from_value(dt.value)
            .index_checked(idx)
            .data()
            .await;
        OutputData {
            value: inner.value,
            secret: secret || inner.secret,
            deps: deps.into_iter().chain(inner.deps).collect(),
        }
    })
}

/// True when a value is the missing-lookup sentinel, looking through the
/// transparent secret and output wrappers a lookup may have added.
fn is_missing(v: &PropertyValue) -> bool {
    match v {
        PropertyValue::Missing | PropertyValue::Failed(_) => true,
        PropertyValue::Secret(inner) => is_missing(inner),
        PropertyValue::Output(o) => o.value.as_deref().is_some_and(is_missing),
        _ => false,
    }
}

/// The failure message a value carries, if its resource registration failed.
fn failure_message(v: &PropertyValue) -> Option<String> {
    match v {
        PropertyValue::Failed(msg) => Some(msg.to_string()),
        PropertyValue::Secret(inner) => failure_message(inner),
        PropertyValue::Output(o) => o.value.as_deref().and_then(failure_message),
        _ => None,
    }
}

/// The `recover` builtin: the value, unless the resource backing it failed
/// to register, in which case the recovery expression evaluated with
/// `error` bound to the failure message.
pub fn recover(
    value: Output<PropertyValue>,
    recovery: impl FnOnce(Output<PropertyValue>) -> Output<PropertyValue> + Send + 'static,
) -> Output<PropertyValue> {
    Output::from_data_future(async move {
        let data = value.data().await;
        match failure_message(&data.value) {
            None => data,
            Some(msg) => {
                let err = Output::from_value(PropertyValue::String(msg));
                // Deliberately drop the failed resource's dependencies: the
                // recovered value must stand on its own.
                let mut recovered = recovery(err).data().await;
                recovered.deps.clear();
                recovered
            }
        }
    })
}

/// The `try` builtin: the first alternative that evaluates without a failed
/// lookup. Secretness and dependencies come from the alternative chosen.
pub fn try_(alts: Vec<Output<PropertyValue>>) -> Output<PropertyValue> {
    Output::from_data_future(async move {
        let mut last = OutputData::from_value(PropertyValue::Null);
        for alt in alts {
            let data = alt.data().await;
            if !is_missing(&data.value) {
                return data;
            }
            last = data;
        }
        // Every alternative failed; surface null rather than the sentinel.
        OutputData {
            value: PropertyValue::Null,
            secret: last.secret,
            deps: last.deps,
        }
    })
}

/// The `can` builtin: whether an expression evaluated without a failed
/// lookup.
pub fn can(v: Output<PropertyValue>) -> Output<PropertyValue> {
    Output::from_data_future(async move {
        let data = v.data().await;
        OutputData {
            value: PropertyValue::Bool(!is_missing(&data.value)),
            secret: data.secret,
            deps: data.deps,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pv;

    async fn value(o: Output<PropertyValue>) -> PropertyValue {
        o.data().await.value
    }

    fn num(n: f64) -> Output<PropertyValue> {
        pv::number(n)
    }

    // --- arithmetic, comparison, logic -------------------------------------

    #[tokio::test]
    async fn arithmetic_and_comparison() {
        assert_eq!(
            value(add(num(2.0), num(3.0))).await,
            PropertyValue::Number(5.0)
        );
        assert_eq!(
            value(sub(num(5.0), num(3.0))).await,
            PropertyValue::Number(2.0)
        );
        assert_eq!(
            value(mul(num(2.0), num(3.0))).await,
            PropertyValue::Number(6.0)
        );
        assert_eq!(
            value(div(num(6.0), num(3.0))).await,
            PropertyValue::Number(2.0)
        );
        assert_eq!(
            value(rem(num(7.0), num(3.0))).await,
            PropertyValue::Number(1.0)
        );
        assert_eq!(value(neg(num(2.0))).await, PropertyValue::Number(-2.0));
        assert_eq!(
            value(lt(num(1.0), num(2.0))).await,
            PropertyValue::Bool(true)
        );
        assert_eq!(
            value(gte(num(2.0), num(2.0))).await,
            PropertyValue::Bool(true)
        );
    }

    #[tokio::test]
    async fn equality_compares_values_not_wrappers() {
        // A secret and a plain value holding the same thing are equal; the
        // secretness rides along on the result instead.
        let d = eq(pv::secret(pv::string("x")), pv::string("x"))
            .data()
            .await;
        assert_eq!(d.value, PropertyValue::Bool(true));
        assert!(d.secret);
        assert_eq!(
            value(neq(pv::string("x"), pv::string("y"))).await,
            PropertyValue::Bool(true)
        );
    }

    #[tokio::test]
    async fn logic_operators() {
        assert_eq!(
            value(and(pv::bool(true), pv::bool(false))).await,
            PropertyValue::Bool(false)
        );
        assert_eq!(
            value(or(pv::bool(true), pv::bool(false))).await,
            PropertyValue::Bool(true)
        );
        assert_eq!(value(not(pv::bool(false))).await, PropertyValue::Bool(true));
    }

    #[tokio::test]
    async fn an_unknown_operand_makes_the_result_unknown() {
        assert!(!add(num(1.0), Output::unknown()).data().await.known());
        assert!(!eq(Output::unknown(), num(1.0)).data().await.known());
    }

    #[tokio::test]
    async fn operators_union_secretness() {
        assert!(add(pv::secret(num(1.0)), num(2.0)).data().await.secret);
    }

    // --- conditional --------------------------------------------------------

    #[tokio::test]
    async fn cond_picks_a_branch_and_carries_the_condition_secret() {
        assert_eq!(
            value(cond(pv::bool(true), pv::string("t"), pv::string("f"))).await,
            PropertyValue::String("t".into())
        );
        assert_eq!(
            value(cond(pv::bool(false), pv::string("t"), pv::string("f"))).await,
            PropertyValue::String("f".into())
        );
        // Which branch was taken leaks the condition, so a secret condition
        // makes the result secret whichever branch wins.
        assert!(
            cond(pv::secret(pv::bool(true)), pv::string("t"), pv::string("f"))
                .data()
                .await
                .secret
        );
    }

    #[tokio::test]
    async fn an_unknown_condition_yields_an_unknown_result() {
        // Not one of the branches: during a preview we do not know which.
        let d = cond(Output::unknown(), pv::string("t"), pv::string("f"))
            .data()
            .await;
        assert!(!d.known());
    }

    // --- coercions ----------------------------------------------------------

    #[tokio::test]
    async fn coercions_follow_pcl_semantics() {
        assert_eq!(
            value(to_number(pv::string("3.5"))).await,
            PropertyValue::Number(3.5)
        );
        assert_eq!(
            value(to_number(pv::bool(true))).await,
            PropertyValue::Number(1.0)
        );
        assert_eq!(
            value(to_int(pv::string("3.9"))).await,
            PropertyValue::Number(3.0)
        );
        assert_eq!(
            value(to_bool(pv::string("true"))).await,
            PropertyValue::Bool(true)
        );
        assert_eq!(
            value(to_string(num(3.0))).await,
            PropertyValue::String("3".into())
        );
        assert_eq!(
            value(to_string(num(3.5))).await,
            PropertyValue::String("3.5".into())
        );
        assert_eq!(
            value(to_string(pv::bool(true))).await,
            PropertyValue::String("true".into())
        );
    }

    #[tokio::test]
    async fn an_uncoercible_value_is_left_alone_rather_than_zeroed() {
        // Turning "abc" into 0 would silently deploy the wrong number.
        assert_eq!(
            value(to_number(pv::string("abc"))).await,
            PropertyValue::String("abc".into())
        );
        assert_eq!(
            value(to_bool(pv::string("yes"))).await,
            PropertyValue::String("yes".into())
        );
    }

    #[tokio::test]
    async fn to_string_leaves_byte_strings_untouched() {
        let bytes = Output::from_value(PropertyValue::ByteString(vec![0xff, 0x00]));
        assert_eq!(
            value(to_string(bytes)).await,
            PropertyValue::ByteString(vec![0xff, 0x00])
        );
    }

    // --- for expressions ----------------------------------------------------

    #[tokio::test]
    async fn for_array_maps_and_filters() {
        let coll = pv::array(vec![num(1.0), num(2.0), num(3.0)]);
        let out = for_array(
            coll,
            // keep elements greater than 1...
            |_k, v| gt(v, num(1.0)),
            // ...and multiply what survives by ten
            |_k, v| mul(v, num(10.0)),
        );
        assert_eq!(
            value(out).await,
            PropertyValue::Array(vec![
                PropertyValue::Number(20.0),
                PropertyValue::Number(30.0)
            ])
        );
    }

    #[tokio::test]
    async fn for_object_keys_by_the_key_expression() {
        let coll = pv::object(vec![("a".to_string(), num(1.0))]);
        let out = for_object(
            coll,
            |_k, _v| pv::bool(true),
            |k, _v| k,
            |_k, v| mul(v, num(2.0)),
        );
        match value(out).await {
            PropertyValue::Object(m) => {
                assert_eq!(m.get("a"), Some(&PropertyValue::Number(2.0)));
            }
            other => panic!("expected an object, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_unknown_filter_condition_makes_the_comprehension_unknown() {
        // Regression: an unknown condition counted as false, so
        // `[for v in list : v if v.enabled]` over a pending resource output
        // previewed as an empty array. The engine then diffed that against
        // the existing list and reported every element as a deletion.
        let out = for_array(
            pv::array(vec![num(1.0), num(2.0)]),
            |_k, _v| Output::unknown(),
            |_k, v| v,
        );
        let d = out.data().await;
        assert!(
            !d.known(),
            "an unknown condition filtered instead of going unknown"
        );

        let out = for_object(
            pv::object(vec![("a".to_string(), num(1.0))]),
            |_k, _v| Output::unknown(),
            |k, _v| k,
            |_k, v| v,
        );
        assert!(!out.data().await.known());
    }

    #[tokio::test]
    async fn a_known_false_condition_still_just_drops_the_element() {
        // The unknown short-circuit must not swallow ordinary filtering.
        let out = for_array(
            pv::array(vec![num(1.0), num(2.0)]),
            |_k, v| gt(v, num(1.0)),
            |_k, v| v,
        );
        assert_eq!(
            value(out).await,
            PropertyValue::Array(vec![PropertyValue::Number(2.0)])
        );
    }

    // --- try / can / recover ------------------------------------------------

    #[tokio::test]
    async fn try_takes_the_first_alternative_that_resolves() {
        let missing = Output::from_value(PropertyValue::Missing);
        let out = try_(vec![missing, pv::string("second")]);
        assert_eq!(value(out).await, PropertyValue::String("second".into()));
    }

    #[tokio::test]
    async fn try_with_no_surviving_alternative_is_null_not_the_sentinel() {
        // The Missing sentinel must never escape into user-visible values.
        let out = try_(vec![Output::from_value(PropertyValue::Missing)]);
        assert_eq!(value(out).await, PropertyValue::Null);
    }

    #[tokio::test]
    async fn can_reports_whether_a_lookup_resolved() {
        assert_eq!(
            value(can(Output::from_value(PropertyValue::Missing))).await,
            PropertyValue::Bool(false)
        );
        assert_eq!(value(can(pv::string("x"))).await, PropertyValue::Bool(true));
        // A null is a real value: `can` is about the lookup, not the content.
        assert_eq!(value(can(pv::null())).await, PropertyValue::Bool(true));
    }

    #[tokio::test]
    async fn recover_passes_a_healthy_value_straight_through() {
        let out = recover(pv::string("fine"), |_e| pv::string("fallback"));
        assert_eq!(value(out).await, PropertyValue::String("fine".into()));
    }

    #[tokio::test]
    async fn recover_substitutes_and_drops_the_failed_dependency() {
        // The recovered value must not depend on the resource that failed, or
        // the engine would refuse to create anything that consumes it.
        let failed = Output::from_data(OutputData {
            value: PropertyValue::Failed("it broke".into()),
            secret: false,
            deps: vec!["urn:dead".into()],
        });
        let out = recover(failed, |e| e);
        let d = out.data().await;
        assert_eq!(d.value, PropertyValue::String("it broke".into()));
        assert!(
            d.deps.is_empty(),
            "the failed resource's deps leaked into the recovery"
        );
    }

    // --- indexing -----------------------------------------------------------

    #[tokio::test]
    async fn index_uses_the_same_pcl_rules_as_output_index() {
        // `index_plain` used to be a second copy of `output::index_value`.
        // These are the rules that had to be kept in step by hand: numeric
        // string keys on arrays, and looking through a secret wrapper while
        // lifting its secretness onto the result.
        let arr = pv::array(vec![num(7.0)]);
        assert_eq!(
            value(index(arr.clone(), pv::string("0"))).await,
            PropertyValue::Number(7.0)
        );
        let d = index(pv::secret(arr), num(0.0)).data().await;
        assert_eq!(d.value, PropertyValue::Number(7.0));
        assert!(d.secret, "indexing a secret container lost its secretness");
    }

    #[tokio::test]
    async fn index_checked_flags_an_absent_key_while_index_nulls_it() {
        let obj = pv::object(vec![("a".to_string(), num(1.0))]);
        assert_eq!(
            value(index(obj.clone(), pv::string("nope"))).await,
            PropertyValue::Null
        );
        assert_eq!(
            value(index_checked(obj, pv::string("nope"))).await,
            PropertyValue::Missing
        );
    }
}
