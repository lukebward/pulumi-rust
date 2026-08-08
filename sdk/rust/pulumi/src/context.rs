//! The deployment context: the SDK's connection to the Pulumi engine.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use futures::future::{BoxFuture, FutureExt, Shared};
use prost_types::Struct;
use tonic::transport::Channel;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::output::{Output, OutputData};
use crate::pulumirpc;
use crate::pulumirpc::engine_client::EngineClient;
use crate::pulumirpc::resource_monitor_client::ResourceMonitorClient;
use crate::value::{marshal_properties, unmarshal_properties, PropertyMap, PropertyValue};

/// Feature flags negotiated with the resource monitor.
#[derive(Clone, Copy, Debug, Default)]
pub struct Features {
    pub secrets: bool,
    pub resource_references: bool,
    pub output_values: bool,
    /// The monitor gates invokes on the created-ness of their declared
    /// dependencies (RESOURCE_MONITOR_FEATURE_INVOKE_DEPENDS_ON).
    pub invoke_depends_on: bool,
}

/// Settings for a program run, prepared by [`crate::runtime::run`].
#[derive(Clone, Debug)]
pub struct RunSettings {
    pub project: String,
    pub stack: String,
    pub organization: String,
    pub dry_run: bool,
    pub monitor_addr: String,
    pub engine_addr: String,
    pub config: HashMap<String, String>,
    pub config_secret_keys: Vec<String>,
}

pub(crate) struct ContextInner {
    pub monitor: ResourceMonitorClient<Channel>,
    pub engine: Option<EngineClient<Channel>>,
    pub settings: RunSettings,
    pub features: Features,
    pub config: Config,
    pub stack_urn: tokio::sync::OnceCell<String>,
    /// In-flight resource registrations the run must drain before finishing.
    pub pending: Mutex<Vec<Shared<BoxFuture<'static, Arc<RegisterOutcome>>>>>,
    /// Stack exports accumulated by [`Context::export`].
    pub exports: Mutex<Vec<(String, Output<PropertyValue>)>>,
}

/// The context handed to a Pulumi program's main function.
#[derive(Clone)]
pub struct Context {
    pub(crate) inner: Arc<ContextInner>,
}

/// The result of a resource registration RPC.
#[derive(Debug)]
pub struct RegisterOutcome {
    pub urn: String,
    pub id: Option<String>,
    pub outputs: PropertyMap,
    pub error: Option<String>,
    /// True when the engine skipped or elided the operation (e.g. targeted
    /// updates); outputs resolve as unknown.
    pub unknown: bool,
}

/// Resource options supported by the SDK.
#[derive(Default, Clone)]
pub struct ResourceOptions {
    pub parent: Option<Resource>,
    pub depends_on: Vec<Resource>,
    pub protect: Option<bool>,
    /// Explicit provider for this resource.
    pub provider: Option<Resource>,
    /// Explicit providers for component resources, keyed by package name.
    pub providers: Vec<(String, Resource)>,
    pub version: String,
    pub plugin_download_url: String,
    pub additional_secret_outputs: Vec<String>,
    pub ignore_changes: Vec<String>,
    pub delete_before_replace: Option<bool>,
    pub retain_on_delete: Option<bool>,
    pub deleted_with: Option<Resource>,
    pub import_id: String,
    pub replace_on_changes: Vec<String>,
    pub custom_timeouts: Option<CustomTimeouts>,
    /// Previous identities for this resource, so the engine treats an
    /// existing resource as this one instead of replacing it.
    pub aliases: Vec<Alias>,
    /// Properties whose diffs the engine hides from the user.
    pub hide_diffs: Vec<String>,
    /// Resources whose replacement forces this resource to be replaced.
    pub replace_with: Vec<Resource>,
    /// A value the engine diffs against its last recorded value; a change
    /// triggers replacement.
    pub replacement_trigger: Option<Output<PropertyValue>>,
    /// Environment-variable remappings for provider resources
    /// (new key -> old key).
    pub env_var_mappings: Vec<(String, String)>,
}

/// A previous identity of a resource.
#[derive(Clone, Debug)]
pub enum Alias {
    /// A fully-specified previous URN.
    Urn(String),
    /// A partial specification; unset parts default to the resource's
    /// current values.
    Spec(AliasSpec),
}

#[derive(Clone, Debug, Default)]
pub struct AliasSpec {
    pub name: Option<String>,
    pub type_: Option<String>,
    pub stack: Option<String>,
    pub project: Option<String>,
    pub parent: Option<AliasParent>,
}

#[derive(Clone, Debug)]
pub enum AliasParent {
    /// The resource previously had this parent.
    Urn(Resource),
    /// The resource previously had no parent.
    None,
}

impl Alias {
    async fn to_proto(&self) -> pulumirpc::Alias {
        use pulumirpc::alias::{spec, Spec};
        let alias = match self {
            Alias::Urn(urn) => pulumirpc::alias::Alias::Urn(urn.clone()),
            Alias::Spec(s) => {
                let parent = match &s.parent {
                    Some(AliasParent::Urn(r)) => {
                        let urn = match r.urn().data().await.value {
                            PropertyValue::String(s) => s,
                            _ => String::new(),
                        };
                        Some(spec::Parent::ParentUrn(urn))
                    }
                    Some(AliasParent::None) => Some(spec::Parent::NoParent(true)),
                    None => None,
                };
                pulumirpc::alias::Alias::Spec(Spec {
                    name: s.name.clone().unwrap_or_default(),
                    r#type: s.type_.clone().unwrap_or_default(),
                    stack: s.stack.clone().unwrap_or_default(),
                    project: s.project.clone().unwrap_or_default(),
                    parent,
                })
            }
        };
        pulumirpc::Alias { alias: Some(alias) }
    }
}

#[derive(Default, Clone)]
pub struct CustomTimeouts {
    pub create: Option<Output<PropertyValue>>,
    pub update: Option<Output<PropertyValue>>,
    pub delete: Option<Output<PropertyValue>>,
    pub read: Option<Output<PropertyValue>>,
}

async fn timeout_str(v: &Option<Output<PropertyValue>>) -> String {
    match v {
        Some(o) => match o.data().await.value {
            PropertyValue::String(s) => s,
            _ => String::new(),
        },
        None => String::new(),
    }
}

/// A request to register a resource, produced by generated SDK code.
pub struct RegisterRequest {
    pub type_: String,
    pub name: String,
    pub custom: bool,
    pub remote: bool,
    pub version: String,
    pub plugin_download_url: String,
    pub inputs: Vec<(String, Output<PropertyValue>)>,
    pub options: ResourceOptions,
}

/// A live reference to a registered (or registering) resource.
#[derive(Clone)]
pub struct Resource {
    state: Shared<BoxFuture<'static, Arc<RegisterOutcome>>>,
    custom: bool,
    dry_run: bool,
}

impl std::fmt::Debug for Resource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Resource<..>")
    }
}

impl Resource {
    /// Identity helper so generated code can treat SDK-typed wrappers and
    /// raw resources uniformly.
    pub fn pulumi_resource(&self) -> &Resource {
        self
    }

    /// The resource's URN.
    pub fn urn(&self) -> Output<String> {
        let state = self.state.clone();
        Output::from_data_future(async move {
            let o = state.await;
            OutputData {
                value: PropertyValue::String(o.urn.clone()),
                secret: false,
                deps: vec![o.urn.clone()],
            }
        })
    }

    /// The resource's provider-assigned ID (custom resources only). Unknown
    /// during previews before the resource is created.
    pub fn id(&self) -> Output<String> {
        let state = self.state.clone();
        Output::from_data_future(async move {
            let o = state.await;
            let value = match &o.id {
                Some(id) if !id.is_empty() && !o.unknown => PropertyValue::String(id.clone()),
                _ => PropertyValue::Computed,
            };
            OutputData { value, secret: false, deps: vec![o.urn.clone()] }
        })
    }

    /// An output property of the resource by its Pulumi (camelCase) name.
    pub fn output(&self, name: &str) -> Output<PropertyValue> {
        let state = self.state.clone();
        let name = name.to_string();
        let dry_run = self.dry_run;
        Output::from_data_future(async move {
            let o = state.await;
            let mut data = match o.outputs.get(&name) {
                Some(v) if !o.unknown => OutputData::from_value(v.clone()),
                _ => OutputData {
                    value: if dry_run || o.unknown {
                        PropertyValue::Computed
                    } else {
                        PropertyValue::Null
                    },
                    secret: false,
                    deps: vec![],
                },
            };
            if !o.urn.is_empty() {
                data.deps.push(o.urn.clone());
            }
            data
        })
    }

    /// A `urn::id` provider reference for explicit-provider options.
    fn provider_ref(&self) -> Output<String> {
        let state = self.state.clone();
        Output::from_data_future(async move {
            let o = state.await;
            let id = match &o.id {
                Some(id) if !id.is_empty() => id.clone(),
                _ => crate::value::UNKNOWN_STRING_VALUE.to_string(),
            };
            OutputData {
                value: PropertyValue::String(format!("{}::{}", o.urn, id)),
                secret: false,
                deps: vec![],
            }
        })
    }
}

/// Options for invoking a provider function.
#[derive(Default, Clone)]
pub struct InvokeOptions {
    pub provider: Option<Resource>,
    pub parent: Option<Resource>,
    pub version: String,
    pub plugin_download_url: String,
    pub depends_on: Vec<Resource>,
}

impl Context {
    /// The current project name.
    pub fn project(&self) -> &str {
        &self.inner.settings.project
    }

    /// The current stack name.
    pub fn stack(&self) -> &str {
        &self.inner.settings.stack
    }

    /// The current organization name.
    pub fn organization(&self) -> &str {
        &self.inner.settings.organization
    }

    /// True when running a preview.
    pub fn dry_run(&self) -> bool {
        self.inner.settings.dry_run
    }

    /// Stack configuration.
    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    /// Export a stack output.
    pub fn export(&self, name: impl Into<String>, value: impl Into<Output<PropertyValue>>) {
        self.inner.exports.lock().unwrap().push((name.into(), value.into()));
    }

    /// Register a resource with the engine. Returns immediately; the
    /// registration proceeds asynchronously and the returned [`Resource`]'s
    /// outputs resolve when it completes.
    pub fn register_resource(&self, req: RegisterRequest) -> Resource {
        let inner = self.inner.clone();
        let dry_run = self.dry_run();
        let custom = req.custom;
        let fut = async move { Arc::new(do_register(inner, req).await) }.boxed().shared();
        // Drive the registration immediately so independent resources
        // register concurrently, then track it for draining at shutdown.
        tokio::spawn(fut.clone());
        self.inner.pending.lock().unwrap().push(fut.clone());
        Resource { state: fut, custom, dry_run }
    }

    /// Check the engine (CLI) version against a semver range, failing the
    /// program when incompatible.
    pub async fn require_pulumi_version(&self, range: Output<PropertyValue>) -> Result<()> {
        let range = match range.data().await.value {
            PropertyValue::String(s) => s,
            _ => return Ok(()),
        };
        if let Some(engine) = &self.inner.engine {
            let mut engine = engine.clone();
            engine
                .require_pulumi_version(pulumirpc::RequirePulumiVersionRequest {
                    pulumi_version_range: range,
                })
                .await
                .map_err(|e| Error::new(e.message().to_string()))?;
        }
        Ok(())
    }

    /// Read an existing resource's state from its provider without managing
    /// it. Returns a resource handle whose outputs are the read state.
    pub fn read_resource(
        &self,
        type_: impl Into<String>,
        name: impl Into<String>,
        id: Output<PropertyValue>,
        inputs: Vec<(String, Output<PropertyValue>)>,
        version: impl Into<String>,
        options: ResourceOptions,
    ) -> Resource {
        let inner = self.inner.clone();
        let dry_run = self.dry_run();
        let type_ = type_.into();
        let name = name.into();
        let version = version.into();
        let fut = async move {
            Arc::new(do_read(inner, type_, name, id, inputs, version, options).await)
        }
        .boxed()
        .shared();
        tokio::spawn(fut.clone());
        self.inner.pending.lock().unwrap().push(fut.clone());
        Resource { state: fut, custom: true, dry_run }
    }

    /// Invoke a provider function, returning its result object as an output.
    ///
    /// If any argument is unknown during a preview, the invoke is skipped and
    /// the result is unknown, mirroring other Pulumi SDKs.
    pub fn invoke(
        &self,
        tok: impl Into<String>,
        args: Vec<(String, Output<PropertyValue>)>,
        opts: InvokeOptions,
    ) -> Output<PropertyValue> {
        let inner = self.inner.clone();
        let tok = tok.into();
        Output::from_data_future(async move {
            match do_invoke(inner.clone(), tok, args, opts).await {
                Ok(data) => data,
                Err(e) => {
                    // An invoke failure is fatal to the program: report it to
                    // the engine and bail with the logged-error exit code.
                    if let Some(engine) = &inner.engine {
                        let mut engine = engine.clone();
                        let _ = engine
                            .log(pulumirpc::LogRequest {
                                severity: pulumirpc::LogSeverity::Error as i32,
                                message: format!("{e}"),
                                ..Default::default()
                            })
                            .await;
                    }
                    std::process::exit(crate::runtime::EXIT_STATUS_LOGGED_ERROR);
                }
            }
        })
    }

    /// Await every outstanding registration, then publish stack outputs.
    /// Outputs are registered even when a registration failed, mirroring the
    /// other SDKs; the first error is returned after outputs are published.
    pub(crate) async fn finish(&self) -> Result<()> {
        // Registrations can enqueue further registrations, so drain in waves.
        let mut first_error: Option<Error> = None;
        loop {
            let batch: Vec<_> = {
                let mut pending = self.inner.pending.lock().unwrap();
                std::mem::take(&mut *pending)
            };
            if batch.is_empty() {
                break;
            }
            for fut in batch {
                let outcome = fut.await;
                if let Some(err) = &outcome.error {
                    if first_error.is_none() {
                        first_error = Some(Error::new(err.clone()));
                    }
                }
            }
        }

        let exports: Vec<_> = {
            let mut exports = self.inner.exports.lock().unwrap();
            std::mem::take(&mut *exports)
        };
        // Stack outputs are encoded without first-class output values,
        // mirroring the Go SDK's RegisterResourceOutputs marshaling. Secret
        // flags survive even when the value is unknown.
        let mut outputs = BTreeMap::new();
        for (name, out) in exports {
            let data = out.data().await;
            let mut value =
                if !data.known() { PropertyValue::Computed } else { data.value };
            if data.secret && self.inner.features.secrets {
                value = PropertyValue::Secret(Box::new(value));
            }
            outputs.insert(name, value);
        }

        let urn = self
            .inner
            .stack_urn
            .get()
            .cloned()
            .ok_or_else(|| Error::new("stack URN not initialized"))?;
        let mut monitor = self.inner.monitor.clone();
        let outputs_result = monitor
            .register_resource_outputs(pulumirpc::RegisterResourceOutputsRequest {
                urn,
                outputs: Some(marshal_properties(&outputs)),
            })
            .await;
        // A registration failure is the root cause; don't let a subsequent
        // outputs-RPC failure mask it.
        match (first_error, outputs_result) {
            (Some(e), _) => Err(e),
            (None, Err(e)) => Err(e.into()),
            (None, Ok(_)) => Ok(()),
        }
    }

    /// Log an error-severity message to the engine.
    pub async fn log_error(&self, message: impl Into<String>) {
        if let Some(engine) = &self.inner.engine {
            let mut engine = engine.clone();
            let _ = engine
                .log(pulumirpc::LogRequest {
                    severity: pulumirpc::LogSeverity::Error as i32,
                    message: message.into(),
                    ..Default::default()
                })
                .await;
        }
    }
}

/// Encode resolved output data as a property value honoring the monitor's
/// negotiated features.
fn encode_value(data: OutputData, features: Features) -> PropertyValue {
    if features.output_values {
        data.into_value()
    } else {
        // Degrade: unknowns become the sentinel, secretness keeps the secret
        // sig, dependencies are carried only out-of-band.
        if !data.known() {
            PropertyValue::Computed
        } else if data.secret && features.secrets {
            PropertyValue::Secret(Box::new(data.value))
        } else {
            data.value
        }
    }
}

async fn await_urn(r: &Resource) -> String {
    match r.urn().data().await.value {
        PropertyValue::String(urn) => urn,
        _ => String::new(),
    }
}

async fn do_register(inner: Arc<ContextInner>, req: RegisterRequest) -> RegisterOutcome {
    let fail = |msg: String| RegisterOutcome {
        urn: String::new(),
        id: None,
        outputs: PropertyMap::new(),
        error: Some(msg),
        unknown: false,
    };

    // Resolve options that reference other resources first. Resources with
    // no explicit parent are parented to the root stack, like other SDKs.
    let parent = match &req.options.parent {
        Some(p) => await_urn(p).await,
        None => inner.stack_urn.get().cloned().unwrap_or_default(),
    };
    let provider = match &req.options.provider {
        Some(p) => match p.provider_ref().data().await.value {
            PropertyValue::String(s) => s,
            _ => String::new(),
        },
        None => String::new(),
    };
    let mut providers = HashMap::new();
    for (pkg, p) in &req.options.providers {
        if let PropertyValue::String(s) = p.provider_ref().data().await.value {
            providers.insert(pkg.clone(), s);
        }
    }
    let deleted_with = match &req.options.deleted_with {
        Some(r) => await_urn(r).await,
        None => String::new(),
    };

    let mut dependencies = BTreeSet::new();
    for dep in &req.options.depends_on {
        dependencies.insert(await_urn(dep).await);
    }

    let mut replace_with = Vec::new();
    for r in &req.options.replace_with {
        let urn = await_urn(r).await;
        if !urn.is_empty() {
            replace_with.push(urn);
        }
    }

    let mut aliases = Vec::new();
    for a in &req.options.aliases {
        aliases.push(a.to_proto().await);
    }

    // The trigger's own dependencies join the resource's, matching the Go
    // SDK; the value keeps unknowns and secrets so the engine can diff it.
    let replacement_trigger = match &req.options.replacement_trigger {
        Some(o) => {
            let data = o.data().await;
            for d in &data.deps {
                dependencies.insert(d.clone());
            }
            match &data.value {
                PropertyValue::Null => None,
                // The trigger is never marshaled as a first-class output
                // value: the engine records it in state and compares it by
                // equality, so dependencies travel only in `dependencies`.
                _ => {
                    let v = if !data.known() {
                        PropertyValue::Computed
                    } else if data.secret && inner.features.secrets {
                        PropertyValue::Secret(Box::new(data.value.clone()))
                    } else {
                        data.value.clone()
                    };
                    Some(v.to_proto())
                }
            }
        }
        None => None,
    };

    let custom_timeouts = match &req.options.custom_timeouts {
        Some(t) => Some(pulumirpc::register_resource_request::CustomTimeouts {
            create: timeout_str(&t.create).await,
            update: timeout_str(&t.update).await,
            delete: timeout_str(&t.delete).await,
            read: timeout_str(&t.read).await,
        }),
        None => None,
    };

    // Await and marshal inputs.
    let mut object = BTreeMap::new();
    let mut property_dependencies = HashMap::new();
    for (key, out) in req.inputs {
        let data = out.data().await;
        for d in &data.deps {
            dependencies.insert(d.clone());
        }
        property_dependencies.insert(
            key.clone(),
            pulumirpc::register_resource_request::PropertyDependencies {
                urns: data.deps.clone(),
            },
        );
        object.insert(key, encode_value(data, inner.features));
    }

    let request = pulumirpc::RegisterResourceRequest {
        r#type: req.type_.clone(),
        name: req.name.clone(),
        parent,
        custom: req.custom,
        object: Some(marshal_properties(&object)),
        protect: req.options.protect,
        dependencies: dependencies.into_iter().filter(|d| !d.is_empty()).collect(),
        provider,
        providers,
        property_dependencies,
        delete_before_replace: req.options.delete_before_replace.unwrap_or(false),
        delete_before_replace_defined: req.options.delete_before_replace.is_some(),
        version: if !req.options.version.is_empty() {
            req.options.version.clone()
        } else {
            req.version.clone()
        },
        ignore_changes: req.options.ignore_changes.clone(),
        accept_secrets: true,
        additional_secret_outputs: req.options.additional_secret_outputs.clone(),
        import_id: req.options.import_id.clone(),
        custom_timeouts: custom_timeouts,
        supports_partial_values: true,
        remote: req.remote,
        accept_resources: true,
        replace_on_changes: req.options.replace_on_changes.clone(),
        plugin_download_url: if !req.options.plugin_download_url.is_empty() {
            req.options.plugin_download_url.clone()
        } else {
            req.plugin_download_url.clone()
        },
        retain_on_delete: req.options.retain_on_delete,
        deleted_with,
        alias_specs: true,
        supports_result_reporting: true,
        accepts_byte_string: true,
        aliases,
        hide_diffs: req.options.hide_diffs.clone(),
        replace_with,
        replacement_trigger,
        env_var_mappings: req.options.env_var_mappings.iter().cloned().collect(),
        ..Default::default()
    };

    let mut monitor = inner.monitor.clone();
    let response = match monitor.register_resource(request).await {
        Ok(r) => r.into_inner(),
        Err(e) => {
            return fail(format!(
                "registering resource {} ({}): {}",
                req.name,
                req.type_,
                e.message()
            ))
        }
    };

    let outputs = match &response.object {
        Some(s) => unmarshal_properties(s),
        None => PropertyMap::new(),
    };
    RegisterOutcome {
        urn: response.urn,
        id: if req.custom { Some(response.id) } else { None },
        outputs,
        error: None,
        unknown: response.unknown,
    }
}

async fn do_read(
    inner: Arc<ContextInner>,
    type_: String,
    name: String,
    id: Output<PropertyValue>,
    inputs: Vec<(String, Output<PropertyValue>)>,
    version: String,
    options: ResourceOptions,
) -> RegisterOutcome {
    let fail = |msg: String| RegisterOutcome {
        urn: String::new(),
        id: None,
        outputs: PropertyMap::new(),
        error: Some(msg),
        unknown: false,
    };

    let id_str = match id.data().await.value {
        PropertyValue::String(s) => s,
        other => {
            return fail(format!("read id must be a string, got {other:?}"));
        }
    };

    let parent = match &options.parent {
        Some(p) => await_urn(p).await,
        None => inner.stack_urn.get().cloned().unwrap_or_default(),
    };

    let mut properties = BTreeMap::new();
    let mut dependencies = BTreeSet::new();
    for (key, out) in inputs {
        let data = out.data().await;
        for d in &data.deps {
            dependencies.insert(d.clone());
        }
        properties.insert(key, encode_value(data, inner.features));
    }

    let request = pulumirpc::ReadResourceRequest {
        id: id_str.clone(),
        r#type: type_.clone(),
        name: name.clone(),
        parent,
        properties: Some(marshal_properties(&properties)),
        dependencies: dependencies.into_iter().filter(|d| !d.is_empty()).collect(),
        version,
        accept_secrets: true,
        accept_resources: true,
        additional_secret_outputs: options.additional_secret_outputs.clone(),
        ..Default::default()
    };

    let mut monitor = inner.monitor.clone();
    let response = match monitor.read_resource(request).await {
        Ok(r) => r.into_inner(),
        Err(e) => {
            return fail(format!("reading resource {name} ({type_}): {}", e.message()));
        }
    };
    let outputs = match &response.properties {
        Some(s) => unmarshal_properties(s),
        None => PropertyMap::new(),
    };
    RegisterOutcome {
        urn: response.urn,
        id: Some(id_str),
        outputs,
        error: None,
        unknown: false,
    }
}

async fn do_invoke(
    inner: Arc<ContextInner>,
    tok: String,
    args: Vec<(String, Output<PropertyValue>)>,
    opts: InvokeOptions,
) -> Result<OutputData> {
    let mut secret = false;
    let mut deps = vec![];
    let mut known = true;
    let mut arg_map = BTreeMap::new();
    for (key, out) in args {
        let data = out.data().await;
        secret |= data.secret;
        deps.extend(data.deps.clone());
        known &= data.known();
        arg_map.insert(key, data.value);
    }

    // Await explicit dependencies before invoking; their URNs become
    // dependencies of the result.
    let mut depends_on = vec![];
    for dep in &opts.depends_on {
        let urn = await_urn(dep).await;
        depends_on.push(urn.clone());
        deps.push(urn);
    }

    // Can't invoke with unknown arguments. On monitors without the
    // INVOKE_DEPENDS_ON gate, conservatively skip previews of invokes that
    // depend on other resources; gating monitors sequence these themselves
    // and answer `unknown` while dependencies are pending.
    if !known
        || (inner.settings.dry_run && !deps.is_empty() && !inner.features.invoke_depends_on)
    {
        return Ok(OutputData { value: PropertyValue::Computed, secret, deps });
    }

    let provider = match &opts.provider {
        Some(p) => match p.provider_ref().data().await.value {
            PropertyValue::String(s) => s,
            _ => String::new(),
        },
        None => String::new(),
    };

    // Advertise every dependency (explicit and argument-derived) so engines
    // that support INVOKE_DEPENDS_ON can sequence the invoke.
    let mut all_depends_on: Vec<String> = depends_on;
    for d in &deps {
        if !all_depends_on.contains(d) {
            all_depends_on.push(d.clone());
        }
    }
    let request = pulumirpc::ResourceInvokeRequest {
        tok: tok.clone(),
        args: Some(marshal_properties(&arg_map)),
        provider,
        version: opts.version.clone(),
        accept_resources: true,
        accepts_byte_string: true,
        plugin_download_url: opts.plugin_download_url.clone(),
        depends_on: all_depends_on,
        ..Default::default()
    };

    let mut monitor = inner.monitor.clone();
    let response = monitor.invoke(request).await.map_err(|e| {
        Error::new(format!("invoking {}: {}", tok, e.message()))
    })?;
    let response = response.into_inner();
    if !response.failures.is_empty() {
        let msgs: Vec<_> = response
            .failures
            .iter()
            .map(|f| {
                if f.property.is_empty() {
                    f.reason.clone()
                } else {
                    format!("{}: {}", f.property, f.reason)
                }
            })
            .collect();
        return Err(Error::new(format!("invoking {}: {}", tok, msgs.join("; "))));
    }
    if response.unknown {
        return Ok(OutputData { value: PropertyValue::Computed, secret, deps });
    }

    let ret = match &response.r#return {
        Some(s) => PropertyValue::Object(unmarshal_properties(s)),
        None => PropertyValue::Object(BTreeMap::new()),
    };
    let data = OutputData::from_value(ret);
    Ok(OutputData {
        value: data.value,
        secret: secret || data.secret,
        deps: deps.into_iter().chain(data.deps).collect(),
    })
}

/// Build a [`Struct`] from marshaled fields — exposed for the runtime module.
pub(crate) fn empty_struct() -> Struct {
    Struct { fields: Default::default() }
}
