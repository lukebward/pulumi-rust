//! Resource lifecycle hooks.
//!
//! A hook is a command the engine runs at a point in a resource's lifecycle.
//! The program registers each hook with the monitor, serving it over the
//! callbacks server, and then names the registered hooks in a resource's
//! options.

use crate::output::Output;
use crate::value::PropertyValue;

/// A registered hook, named in a resource's [`ResourceHookBinding`].
#[derive(Clone, Debug)]
pub struct ResourceHook {
    pub(crate) name: String,
}

impl ResourceHook {
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// The hooks bound to a resource, by lifecycle point.
#[derive(Clone, Debug, Default)]
pub struct ResourceHookBinding {
    pub before_create: Vec<ResourceHook>,
    pub after_create: Vec<ResourceHook>,
    pub before_update: Vec<ResourceHook>,
    pub after_update: Vec<ResourceHook>,
    pub before_delete: Vec<ResourceHook>,
    pub after_delete: Vec<ResourceHook>,
    pub on_error: Vec<ResourceHook>,
}

impl ResourceHookBinding {
    pub(crate) fn to_proto(
        &self,
    ) -> crate::pulumirpc::register_resource_request::ResourceHooksBinding {
        let names = |hooks: &Vec<ResourceHook>| -> Vec<String> {
            hooks.iter().map(|h| h.name.clone()).collect()
        };
        crate::pulumirpc::register_resource_request::ResourceHooksBinding {
            before_create: names(&self.before_create),
            after_create: names(&self.after_create),
            before_update: names(&self.before_update),
            after_update: names(&self.after_update),
            before_delete: names(&self.before_delete),
            after_delete: names(&self.after_delete),
            on_error: names(&self.on_error),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.before_create.is_empty()
            && self.after_create.is_empty()
            && self.before_update.is_empty()
            && self.after_update.is_empty()
            && self.before_delete.is_empty()
            && self.after_delete.is_empty()
            && self.on_error.is_empty()
    }
}

/// Run a hook's command, returning the error message the engine should see
/// when it fails.
pub(crate) async fn run_command(argv: Output<PropertyValue>) -> Option<String> {
    let value = argv.data().await.value;
    // pv::array re-attaches a secret/output envelope to any element that is
    // secret or carries dependencies; strip those before rendering, or the
    // command sees a debug dump instead of its argument.
    fn unwrap(v: &PropertyValue) -> PropertyValue {
        match v {
            PropertyValue::Secret(inner) => unwrap(inner),
            PropertyValue::Output(o) => match &o.value {
                Some(inner) => unwrap(inner),
                None => PropertyValue::Computed,
            },
            other => other.clone(),
        }
    }
    let parts: Vec<String> = match unwrap(&value) {
        PropertyValue::Array(items) => items
            .iter()
            .map(|v| crate::output::display(&unwrap(v)))
            .collect(),
        other => vec![crate::output::display(&other)],
    };
    let Some((program, args)) = parts.split_first() else {
        return Some("hook command is empty".to_string());
    };
    match tokio::process::Command::new(program)
        .args(args)
        .status()
        .await
    {
        Ok(status) if status.success() => None,
        Ok(status) => Some(format!("command {:?} failed: {}", parts, status)),
        Err(e) => Some(format!("running command {:?}: {}", parts, e)),
    }
}
