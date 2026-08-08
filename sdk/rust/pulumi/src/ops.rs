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
        OutputData { value: f(da.value, db.value), secret, deps }
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

/// Index with a dynamic key.
pub fn index(target: Output<PropertyValue>, key: Output<PropertyValue>) -> Output<PropertyValue> {
    combine2(target, key, |t, k| {
        let idx = match &k {
            PropertyValue::Number(n) => crate::output::PropIndex::Index(*n as usize),
            PropertyValue::String(s) => crate::output::PropIndex::Key(s.clone()),
            _ => return PropertyValue::Null,
        };
        index_plain(&t, &idx)
    })
}

fn index_plain(v: &PropertyValue, key: &crate::output::PropIndex) -> PropertyValue {
    use crate::output::PropIndex;
    match (v, key) {
        (PropertyValue::Secret(inner), _) => {
            PropertyValue::Secret(Box::new(index_plain(inner, key)))
        }
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
