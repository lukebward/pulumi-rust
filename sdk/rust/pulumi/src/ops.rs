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
            return OutputData { value: PropertyValue::Computed, secret, deps };
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
    combine2(a, b, |a, b| PropertyValue::Number(as_number(&a) % as_number(&b)))
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
    a.map(|v: PropertyValue| PropertyValue::Bool(!as_bool(&v))).cast()
}

pub fn neg(a: Output<PropertyValue>) -> Output<PropertyValue> {
    a.map(|v: PropertyValue| PropertyValue::Number(-as_number(&v))).cast()
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
            return OutputData { value: PropertyValue::Computed, secret: dc.secret, deps: dc.deps };
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

/// Index with a dynamic key. A container with unknown elements can still be
/// indexed; only a wholly-unknown container (or key) is opaque.
pub fn index(target: Output<PropertyValue>, key: Output<PropertyValue>) -> Output<PropertyValue> {
    Output::from_data_future(async move {
        let dt = target.data().await;
        let dk = key.data().await;
        let secret = dt.secret || dk.secret;
        let deps: Vec<String> = dt.deps.iter().chain(dk.deps.iter()).cloned().collect();
        if matches!(dt.value, PropertyValue::Computed) || !dk.known() {
            return OutputData { value: PropertyValue::Computed, secret, deps };
        }
        let idx = match &dk.value {
            PropertyValue::Number(n) => crate::output::PropIndex::Index(*n as usize),
            PropertyValue::String(s) => crate::output::PropIndex::Key(s.clone()),
            _ => {
                return OutputData { value: PropertyValue::Null, secret, deps };
            }
        };
        let inner = OutputData::from_value(index_plain(&dt.value, &idx));
        OutputData {
            value: inner.value,
            secret: secret || inner.secret,
            deps: deps.into_iter().chain(inner.deps).collect(),
        }
    })
}

fn index_plain(v: &PropertyValue, key: &crate::output::PropIndex) -> PropertyValue {
    use crate::output::PropIndex;
    match (v, key) {
        (PropertyValue::Secret(inner), _) => {
            PropertyValue::Secret(Box::new(index_plain(inner, key)))
        }
        (PropertyValue::Output(o), _) => match &o.value {
            Some(inner) => {
                let mut out = o.clone();
                out.value = Some(Box::new(index_plain(inner, key)));
                PropertyValue::Output(out)
            }
            None => v.clone(),
        },
        (PropertyValue::Object(m), PropIndex::Key(k)) => {
            m.get(k).cloned().unwrap_or(PropertyValue::Null)
        }
        (PropertyValue::Array(a), PropIndex::Index(i)) => {
            a.get(*i).cloned().unwrap_or(PropertyValue::Null)
        }
        (PropertyValue::Array(a), PropIndex::Key(k)) => match k.parse::<usize>() {
            Ok(i) => a.get(i).cloned().unwrap_or(PropertyValue::Null),
            Err(_) => PropertyValue::Null,
        },
        (PropertyValue::Object(m), PropIndex::Index(i)) => {
            m.get(&i.to_string()).cloned().unwrap_or(PropertyValue::Null)
        }
        _ => PropertyValue::Null,
    }
}
