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
#[derive(Clone, Debug, Default)]
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
    /// The callbacks server, started lazily the first time a hook is
    /// registered.
    pub callbacks: tokio::sync::OnceCell<Arc<crate::callbacks::CallbackServer>>,
    /// Package references handed out by RegisterPackage, memoized so each
    /// parameterized package registers exactly once.
    pub package_refs: tokio::sync::Mutex<HashMap<PackageDescriptor, String>>,
    /// Resource states fetched through the builtin getResource, by URN.
    pub hydrated: tokio::sync::Mutex<HashMap<String, PropertyValue>>,
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
    /// True when the engine reported the registration itself failed. Under
    /// continue-on-error the program keeps running, and `recover` turns
    /// this into a fallback value.
    pub failed: Option<String>,
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
    /// An explicit provider that arrived as a value rather than a resource,
    /// e.g. one returned from a resource method.
    pub provider_value: Option<Output<PropertyValue>>,
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
    /// Lifecycle hooks bound to this resource.
    pub hooks: crate::hooks::ResourceHookBinding,
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

/// Work out which providers a registration inherits and which one actually
/// serves it, the way Go's `mergeProviders` and `getProvider` do together.
///
/// The map is what children and invokes parented here will inherit: the
/// parent's map, then this call's explicit `providers`, then the singular
/// `provider` under the package it serves. That last insert is tested
/// against the *explicit* map rather than the merged one, so an entry this
/// call named wins but one merely inherited from the parent is overridden.
///
/// The resolved provider is the singular option only when it serves the
/// resource's own package; otherwise the map decides. A provider for another
/// package is not applicable, and sending it would route the resource to the
/// wrong plugin.
fn resolve_providers(
    type_: &str,
    options: &ResourceOptions,
) -> (BTreeMap<String, Resource>, Option<Resource>) {
    let mut providers: BTreeMap<String, Resource> = match &options.parent {
        Some(p) => p.providers.as_ref().clone(),
        None => BTreeMap::new(),
    };
    for (pkg, p) in &options.providers {
        providers.insert(pkg.clone(), p.clone());
    }
    if let Some(p) = &options.provider {
        if !p.package.is_empty() && !options.providers.iter().any(|(k, _)| k == &p.package) {
            providers.insert(p.package.clone(), p.clone());
        }
    }
    let package = type_.split(':').next().unwrap_or_default();
    let resolved = match &options.provider {
        Some(p) if p.package == package => Some(p.clone()),
        _ => providers.get(package).cloned(),
    };
    (providers, resolved)
}

/// The package a provider URN serves, or the empty string if the URN is not
/// a provider's. A provider's type token is `pulumi:providers:<package>`, so
/// the package is the last segment of the URN's type.
fn provider_package_of_urn(urn: &str) -> String {
    // urn:pulumi:<stack>::<project>::<type>::<name>
    let type_ = match urn.split("::").nth(2) {
        Some(t) => t,
        None => return String::new(),
    };
    match type_.strip_prefix("pulumi:providers:") {
        Some(pkg) => pkg.to_string(),
        None => String::new(),
    }
}

/// A request to register a resource, produced by generated SDK code.
///
/// `Default` is derived for the benefit of hand-written programs, which
/// would otherwise fail to compile every time a field is added. Generated
/// code names every field deliberately, so that the generator's own output
/// is a compile error when it falls behind this struct.
#[derive(Default)]
pub struct RegisterRequest {
    pub type_: String,
    pub name: String,
    pub custom: bool,
    pub remote: bool,
    pub version: String,
    pub plugin_download_url: String,
    pub inputs: Vec<(String, Output<PropertyValue>)>,
    pub options: ResourceOptions,
    /// The parameterized package this resource belongs to, if any. The
    /// package is registered once per program and its reference travels with
    /// every registration.
    pub package: Option<PackageDescriptor>,
    /// Inputs that must not be awaited while registering, because they come
    /// from another component that is itself waiting on this one. Their
    /// values still flow to the component's children.
    pub deferred_inputs: Vec<String>,
    /// Wire names of the inputs the schema marks required. Every generated
    /// args field is an `Option` so the struct can derive `Default`, which
    /// means required-ness is checked here rather than by the compiler.
    pub required: &'static [&'static str],
}

/// A parameterized package: a base plugin plus the parameter that turns it
/// into the package the program uses.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct PackageDescriptor {
    pub base_name: String,
    pub base_version: String,
    pub download_url: String,
    pub name: String,
    pub version: String,
    /// The parameter value, base64 encoded as it appears in the schema.
    pub base64_parameter: String,
    /// True for an extension parameterization layered onto the base
    /// provider, false for a replacement parameterization.
    pub extension: bool,
}

/// A live reference to a registered (or registering) resource.
#[derive(Clone)]
pub struct Resource {
    state: Shared<BoxFuture<'static, Arc<RegisterOutcome>>>,
    custom: bool,
    dry_run: bool,
    /// The provider, version and plugin URL this resource was registered
    /// with, so a method call on it reaches the same provider.
    provider: Option<Arc<Resource>>,
    version: String,
    plugin_download_url: String,
    /// The providers this resource was registered with, so children and
    /// invokes parented to it resolve the same providers.
    providers: Arc<BTreeMap<String, Resource>>,
    /// For a provider resource, the package it serves; empty otherwise. Go
    /// keys its providers map by this, and uses it to decide whether an
    /// explicit `provider` option is even applicable to a resource.
    package: String,
}

impl std::fmt::Debug for Resource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Resource<..>")
    }
}

impl Resource {
    /// Identity helper so generated code can treat SDK-typed wrappers and
    /// raw resources uniformly.
    /// The providers this resource carries, by package name.
    pub fn pulumi_providers(&self) -> &BTreeMap<String, Resource> {
        &self.providers
    }

    pub fn pulumi_resource(&self) -> &Resource {
        self
    }

    /// The resource's URN.
    pub fn urn(&self) -> Output<String> {
        let state = self.state.clone();
        Output::from_data_future(async move {
            let o = state.await;
            if let Some(msg) = &o.failed {
                return OutputData {
                    value: PropertyValue::Failed(msg.as_str().into()),
                    secret: false,
                    deps: if o.urn.is_empty() {
                        vec![]
                    } else {
                        vec![o.urn.clone()]
                    },
                };
            }
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
            OutputData {
                value,
                secret: false,
                deps: vec![o.urn.clone()],
            }
        })
    }

    /// An output property of the resource by its Pulumi (camelCase) name.
    pub fn output(&self, name: &str) -> Output<PropertyValue> {
        let state = self.state.clone();
        let name = name.to_string();
        let dry_run = self.dry_run;
        Output::from_data_future(async move {
            let o = state.await;
            if let Some(msg) = &o.failed {
                // Keep the dependency: under continue-on-error the engine
                // must still skip whatever consumes a failed resource.
                // `recover` drops it explicitly for the value it recovers.
                return OutputData {
                    value: PropertyValue::Failed(msg.as_str().into()),
                    secret: false,
                    deps: if o.urn.is_empty() {
                        vec![]
                    } else {
                        vec![o.urn.clone()]
                    },
                };
            }
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

    /// The resource as a first-class reference value, for inputs typed as a
    /// resource. Deliberately carries no dependencies: the other SDKs
    /// exclude resource references from property dependencies, and the
    /// engine relies on that.
    pub fn reference(&self) -> Output<PropertyValue> {
        let state = self.state.clone();
        let custom = self.custom;
        let version = self.version.clone();
        Output::from_data_future(async move {
            let o = state.await;
            let value = PropertyValue::ResourceReference(crate::value::ResourceReference {
                urn: o.urn.clone(),
                id: if custom {
                    Some(o.id.clone().filter(|i| !i.is_empty()))
                } else {
                    None
                },
                package_version: version,
            });
            OutputData {
                value,
                secret: false,
                deps: vec![],
            }
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
    /// The parameterized package providing this function, if any.
    pub package: Option<PackageDescriptor>,
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
        self.inner
            .exports
            .lock()
            .unwrap()
            .push((name.into(), value.into()));
    }

    /// Register a resource with the engine. Returns immediately; the
    /// registration proceeds asynchronously and the returned [`Resource`]'s
    /// outputs resolve when it completes.
    pub fn register_resource(&self, req: RegisterRequest) -> Resource {
        let inner = self.inner.clone();
        let dry_run = self.dry_run();
        let custom = req.custom;
        let version = if req.options.version.is_empty() {
            req.version.clone()
        } else {
            req.options.version.clone()
        };
        let plugin_download_url = if req.options.plugin_download_url.is_empty() {
            req.plugin_download_url.clone()
        } else {
            req.options.plugin_download_url.clone()
        };
        let (providers, resolved) = resolve_providers(&req.type_, &req.options);
        let providers = Arc::new(providers);
        let provider = resolved.map(Arc::new);
        // For a provider resource, the package it serves.
        let serves = req
            .type_
            .strip_prefix("pulumi:providers:")
            .unwrap_or_default()
            .to_string();
        // The resolved provider is what travels on the wire too, so a
        // mismatched explicit provider is discarded rather than routing the
        // resource to the wrong plugin, and a provider inherited from the map
        // is actually sent.
        let mut req = req;
        if req.options.provider_value.is_none() {
            req.options.provider = provider.as_ref().map(|p| (**p).clone());
        }
        let fut = async move { Arc::new(do_register(inner, req).await) }
            .boxed()
            .shared();
        // Drive the registration immediately so independent resources
        // register concurrently, then track it for draining at shutdown.
        tokio::spawn(fut.clone());
        self.inner.pending.lock().unwrap().push(fut.clone());
        Resource {
            state: fut,
            custom,
            dry_run,
            provider,
            version,
            plugin_download_url,
            providers,
            package: serves,
        }
    }

    /// A handle to an already-registered resource identified by its URN,
    /// used when the engine hands a provider the parent of a component it
    /// is constructing.
    pub fn resource_from_urn(&self, urn: &str) -> Resource {
        let urn = urn.to_string();
        let package = provider_package_of_urn(&urn);
        let dry_run = self.dry_run();
        let fut = async move {
            Arc::new(RegisterOutcome {
                urn,
                id: None,
                outputs: PropertyMap::new(),
                error: None,
                failed: None,
                unknown: false,
            })
        }
        .boxed()
        .shared();
        Resource {
            state: fut,
            custom: false,
            dry_run,
            provider: None,
            version: String::new(),
            plugin_download_url: String::new(),
            providers: Arc::new(BTreeMap::new()),
            package,
        }
    }

    /// A handle to a provider named by a `urn::id` reference, as the engine
    /// passes them to a component provider's Construct.
    pub fn provider_from_reference(&self, reference: &str) -> Resource {
        let (urn, id) = match reference.rsplit_once("::") {
            Some((urn, id)) => (urn.to_string(), id.to_string()),
            None => (reference.to_string(), String::new()),
        };
        let package = provider_package_of_urn(&urn);
        let dry_run = self.dry_run();
        let fut = async move {
            Arc::new(RegisterOutcome {
                urn,
                id: Some(id),
                outputs: PropertyMap::new(),
                error: None,
                failed: None,
                unknown: false,
            })
        }
        .boxed()
        .shared();
        Resource {
            state: fut,
            custom: true,
            dry_run,
            provider: None,
            version: String::new(),
            plugin_download_url: String::new(),
            providers: Arc::new(BTreeMap::new()),
            package,
        }
    }

    /// Wait for every registration started so far, without publishing stack
    /// outputs. A component provider uses this before answering Construct.
    pub async fn drain(&self) -> Result<()> {
        loop {
            let batch: Vec<_> = {
                let mut pending = self.inner.pending.lock().unwrap();
                std::mem::take(&mut *pending)
            };
            if batch.is_empty() {
                return Ok(());
            }
            for fut in batch {
                let outcome = fut.await;
                if let Some(err) = &outcome.error {
                    return Err(Error::new(err.clone()));
                }
            }
        }
    }

    /// Publish a component's outputs. Tracked like a registration so the
    /// program does not exit before it completes.
    pub fn register_resource_outputs(
        &self,
        resource: &Resource,
        outputs: Vec<(String, Output<PropertyValue>)>,
    ) {
        let inner = self.inner.clone();
        let state = resource.state.clone();
        let secrets = self.inner.features.secrets;
        let fut = async move {
            let outcome = state.await;
            let mut props = BTreeMap::new();
            for (name, out) in outputs {
                let data = out.data().await;
                let mut value = if !data.known() {
                    PropertyValue::Computed
                } else {
                    data.value
                };
                if data.secret && secrets {
                    value = PropertyValue::Secret(Box::new(value));
                }
                props.insert(name, value);
            }
            let mut monitor = inner.monitor.clone();
            let error = monitor
                .register_resource_outputs(pulumirpc::RegisterResourceOutputsRequest {
                    urn: outcome.urn.clone(),
                    outputs: Some(marshal_properties(&props)),
                })
                .await
                .err()
                .map(|e| format!("registering outputs for {}: {}", outcome.urn, e.message()));
            Arc::new(RegisterOutcome {
                urn: outcome.urn.clone(),
                id: None,
                outputs: PropertyMap::new(),
                error,
                failed: None,
                unknown: false,
            })
        }
        .boxed()
        .shared();
        tokio::spawn(fut.clone());
        self.inner.pending.lock().unwrap().push(fut);
    }

    /// Register a resource lifecycle hook, returning a handle to name in a
    /// resource's options. The command closure receives the hook arguments
    /// (urn, id, name, type and the resource's inputs and outputs) and
    /// yields the argv to run.
    pub async fn register_resource_hook(
        &self,
        name: impl Into<String>,
        on_dry_run: bool,
        ignore_errors: bool,
        command: impl Fn(Output<PropertyValue>) -> Output<PropertyValue> + Send + Sync + 'static,
    ) -> Result<crate::hooks::ResourceHook> {
        let name = name.into();
        let server = self.callback_server().await?;
        let command = Arc::new(command);
        let callback = server.register(Arc::new(move |bytes: Vec<u8>| {
            let command = command.clone();
            Box::pin(async move {
                let request =
                    <pulumirpc::ResourceHookRequest as prost::Message>::decode(bytes.as_slice())
                        .map_err(|e| tonic::Status::invalid_argument(format!("{e}")))?;
                let args = hook_args(
                    &request.urn,
                    &request.id,
                    &request.name,
                    &request.r#type,
                    request.new_inputs.as_ref(),
                    request.old_inputs.as_ref(),
                    request.new_outputs.as_ref(),
                    request.old_outputs.as_ref(),
                );
                let error = crate::hooks::run_command(command(args))
                    .await
                    .unwrap_or_default();
                Ok(prost::Message::encode_to_vec(
                    &pulumirpc::ResourceHookResponse { error },
                ))
            })
        }));

        let mut monitor = self.inner.monitor.clone();
        monitor
            .register_resource_hook(pulumirpc::RegisterResourceHookRequest {
                name: name.clone(),
                callback: Some(callback),
                on_dry_run,
                ignore_errors,
            })
            .await
            .map_err(|e| Error::new(format!("registering hook {name}: {}", e.message())))?;
        Ok(crate::hooks::ResourceHook { name })
    }

    /// Register an error hook, run when a resource operation fails.
    pub async fn register_error_hook(
        &self,
        name: impl Into<String>,
        command: impl Fn(Output<PropertyValue>) -> Output<PropertyValue> + Send + Sync + 'static,
    ) -> Result<crate::hooks::ResourceHook> {
        let name = name.into();
        let server = self.callback_server().await?;
        let command = Arc::new(command);
        let callback = server.register(Arc::new(move |bytes: Vec<u8>| {
            let command = command.clone();
            Box::pin(async move {
                let request =
                    <pulumirpc::ErrorHookRequest as prost::Message>::decode(bytes.as_slice())
                        .map_err(|e| tonic::Status::invalid_argument(format!("{e}")))?;
                let args = hook_args(
                    &request.urn,
                    &request.id,
                    &request.name,
                    &request.r#type,
                    request.new_inputs.as_ref(),
                    request.old_inputs.as_ref(),
                    None,
                    request.old_outputs.as_ref(),
                );
                // The operation is retried if and only if the hook's command
                // exits successfully; a failing command is not a hook error.
                let failed = crate::hooks::run_command(command(args)).await;
                Ok(prost::Message::encode_to_vec(
                    &pulumirpc::ErrorHookResponse {
                        error: String::new(),
                        retry: failed.is_none(),
                    },
                ))
            })
        }));

        let mut monitor = self.inner.monitor.clone();
        monitor
            .register_error_hook(pulumirpc::RegisterErrorHookRequest {
                name: name.clone(),
                callback: Some(callback),
            })
            .await
            .map_err(|e| Error::new(format!("registering error hook {name}: {}", e.message())))?;
        Ok(crate::hooks::ResourceHook { name })
    }

    async fn callback_server(&self) -> Result<Arc<crate::callbacks::CallbackServer>> {
        self.inner
            .callbacks
            .get_or_try_init(|| async {
                crate::callbacks::CallbackServer::start()
                    .await
                    .map(Arc::new)
            })
            .await
            .cloned()
    }

    /// Call a method on a resource. The receiver travels as `__self__`
    /// alongside the arguments, and the method runs on the provider that
    /// created the receiver.
    pub fn call(
        &self,
        tok: impl Into<String>,
        self_: &Resource,
        args: Vec<(String, Output<PropertyValue>)>,
    ) -> Output<PropertyValue> {
        let inner = self.inner.clone();
        let tok = tok.into();
        let self_ = self_.clone();
        Output::from_data_future(async move {
            match do_call(inner.clone(), tok, self_, args).await {
                Ok(data) => data,
                Err(e) => {
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
        // A read resource is never a provider itself, but it is read
        // *through* one, and its children inherit that.
        let package = String::new();
        let (providers, resolved) = resolve_providers(&type_, &options);
        let providers = Arc::new(providers);
        let provider = resolved.clone().map(Arc::new);
        let kept_version = version.clone();
        let fut = async move {
            Arc::new(do_read(inner, type_, name, id, inputs, version, resolved, options).await)
        }
        .boxed()
        .shared();
        tokio::spawn(fut.clone());
        self.inner.pending.lock().unwrap().push(fut.clone());
        Resource {
            state: fut,
            custom: true,
            dry_run,
            provider,
            version: kept_version,
            plugin_download_url: String::new(),
            providers,
            package,
        }
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
            let mut value = if !data.known() {
                PropertyValue::Computed
            } else {
                data.value
            };
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
        return data.into_value();
    }
    // Degrade: unknowns become the sentinel, secretness keeps the secret
    // sig, dependencies are carried only out-of-band.
    let mut value = if !data.known() {
        PropertyValue::Computed
    } else {
        strip_output_values(data.value, features)
    };
    // Secretness survives an unknown value. Dropping it here disagreed with
    // `Context::finish`, which wraps the very same unknown-and-secret case
    // as Secret(Computed): one secret export marshaled as a secret and the
    // identical value marshaled plain when it was a resource input.
    if data.secret && features.secrets {
        value = PropertyValue::Secret(Box::new(value));
    }
    value
}

/// Rewrite first-class output values into what a monitor that never
/// negotiated them can read.
///
/// Only the top level used to be unwrapped, but `output::all` and
/// `output::object` embed `PropertyValue::Output` on the *elements* so that
/// partially-known collections keep their per-element flags. Those nested
/// wrappers marshaled with `OUTPUT_VALUE_SIG` to a monitor that had not
/// asked for it, which reads back as a plain object with a `4dabf18…` key.
/// Dependencies are dropped — without output values they travel only in the
/// registration's dependency lists.
fn strip_output_values(v: PropertyValue, features: Features) -> PropertyValue {
    let secret_wrap = |inner: PropertyValue, secret: bool| {
        if secret && features.secrets {
            PropertyValue::Secret(Box::new(inner))
        } else {
            inner
        }
    };
    match v {
        PropertyValue::Output(o) => {
            let inner = match o.value {
                Some(inner) => strip_output_values(*inner, features),
                // No value at all means unknown; the sentinel is the only
                // way to say so without output values.
                None => PropertyValue::Computed,
            };
            secret_wrap(inner, o.secret)
        }
        PropertyValue::Secret(inner) => secret_wrap(strip_output_values(*inner, features), true),
        PropertyValue::Array(vs) => PropertyValue::Array(
            vs.into_iter()
                .map(|v| strip_output_values(v, features))
                .collect(),
        ),
        PropertyValue::Object(m) => PropertyValue::Object(
            m.into_iter()
                .map(|(k, v)| (k, strip_output_values(v, features)))
                .collect(),
        ),
        other => other,
    }
}

/// Render a `urn::id` provider reference from a value holding a resource
/// reference, e.g. a provider returned from a resource method.
fn provider_ref_from_value(v: &PropertyValue) -> String {
    match v {
        PropertyValue::ResourceReference(r) => {
            let id = match &r.id {
                Some(Some(id)) if !id.is_empty() => id.clone(),
                _ => crate::value::UNKNOWN_STRING_VALUE.to_string(),
            };
            format!("{}::{}", r.urn, id)
        }
        PropertyValue::Secret(inner) => provider_ref_from_value(inner),
        PropertyValue::Output(o) => o
            .value
            .as_deref()
            .map(provider_ref_from_value)
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Register a parameterized package, memoizing the reference the engine
/// hands back so repeated registrations reuse it.
async fn package_ref(inner: &Arc<ContextInner>, pkg: &PackageDescriptor) -> String {
    {
        let refs = inner.package_refs.lock().await;
        if let Some(r) = refs.get(pkg) {
            return r.clone();
        }
    }
    use base64::Engine;
    let value = base64::engine::general_purpose::STANDARD
        .decode(&pkg.base64_parameter)
        .unwrap_or_default();
    let parameterization = pulumirpc::Parameterization {
        name: pkg.name.clone(),
        version: pkg.version.clone(),
        value,
    };
    let request = pulumirpc::RegisterPackageRequest {
        name: pkg.base_name.clone(),
        version: pkg.base_version.clone(),
        download_url: pkg.download_url.clone(),
        parameterization: if pkg.extension {
            None
        } else {
            Some(parameterization.clone())
        },
        extension: if pkg.extension {
            Some(parameterization)
        } else {
            None
        },
        ..Default::default()
    };
    let mut monitor = inner.monitor.clone();
    let reference = match monitor.register_package(request).await {
        Ok(r) => r.into_inner().r#ref,
        Err(_) => String::new(),
    };
    let mut refs = inner.package_refs.lock().await;
    refs.insert(pkg.clone(), reference.clone());
    reference
}

/// The running program's context, so value-level operations that need the
/// monitor (hydrating a resource reference) can reach it. A program has
/// exactly one context.
static ACTIVE: std::sync::OnceLock<Arc<ContextInner>> = std::sync::OnceLock::new();

pub(crate) fn set_active(inner: Arc<ContextInner>) {
    let _ = ACTIVE.set(inner);
}

/// Fetch a referenced resource's outputs through the engine's built-in
/// `getResource` function, so a program can read properties off a resource
/// reference it received from a component.
pub(crate) async fn hydrate(v: PropertyValue) -> PropertyValue {
    let r = match &v {
        PropertyValue::ResourceReference(r) => r.clone(),
        _ => return v,
    };
    match resource_state(&r.urn).await {
        Some(state) => state,
        None => v,
    }
}

/// Touch a resource reference so its resource is hydrated, but keep the
/// value as the reference. Forwarding a reference to another resource must
/// still send a reference, yet the engine only stands up the builtin
/// provider once a program actually asks for a resource's state.
pub(crate) async fn touch_reference(v: PropertyValue) -> PropertyValue {
    if let PropertyValue::ResourceReference(r) = &v {
        let _ = resource_state(&r.urn).await;
    }
    v
}

/// Fetch a resource's outputs through the engine's built-in `getResource`,
/// once per URN.
async fn resource_state(urn: &str) -> Option<PropertyValue> {
    let inner = ACTIVE.get().cloned()?;
    {
        let cache = inner.hydrated.lock().await;
        if let Some(v) = cache.get(urn) {
            return Some(v.clone());
        }
    }
    let args = vec![(
        "urn".to_string(),
        Output::from_value(PropertyValue::String(urn.to_string())),
    )];
    let data = do_invoke(
        inner.clone(),
        "pulumi:pulumi:getResource".to_string(),
        args,
        InvokeOptions::default(),
    )
    .await
    .ok()?;
    let state = match data.value {
        PropertyValue::Object(m) => m.get("state").cloned()?,
        _ => return None,
    };
    inner
        .hydrated
        .lock()
        .await
        .insert(urn.to_string(), state.clone());
    Some(state)
}

/// Build the `args` object a hook command sees.
#[allow(clippy::too_many_arguments)]
fn hook_args(
    urn: &str,
    id: &str,
    name: &str,
    type_: &str,
    new_inputs: Option<&prost_types::Struct>,
    old_inputs: Option<&prost_types::Struct>,
    new_outputs: Option<&prost_types::Struct>,
    old_outputs: Option<&prost_types::Struct>,
) -> Output<PropertyValue> {
    let mut m = BTreeMap::new();
    m.insert("urn".to_string(), PropertyValue::String(urn.to_string()));
    m.insert("id".to_string(), PropertyValue::String(id.to_string()));
    m.insert("name".to_string(), PropertyValue::String(name.to_string()));
    m.insert("type".to_string(), PropertyValue::String(type_.to_string()));
    let mut put = |key: &str, s: Option<&prost_types::Struct>| {
        if let Some(s) = s {
            m.insert(
                key.to_string(),
                PropertyValue::Object(unmarshal_properties(s)),
            );
        }
    };
    put("newInputs", new_inputs);
    put("oldInputs", old_inputs);
    put("newOutputs", new_outputs);
    put("oldOutputs", old_outputs);
    Output::from_value(PropertyValue::Object(m))
}

async fn await_urn(r: &Resource) -> String {
    match r.urn().data().await.value {
        PropertyValue::String(urn) => urn,
        _ => String::new(),
    }
}

/// An outcome for a resource that was never registered because something it
/// depends on failed.
fn propagated(msg: String) -> RegisterOutcome {
    RegisterOutcome {
        urn: String::new(),
        id: None,
        outputs: PropertyMap::new(),
        error: None,
        failed: Some(msg),
        unknown: true,
    }
}

/// The failure message carried by a value whose resource failed.
fn failure_of(v: &PropertyValue) -> Option<String> {
    match v {
        PropertyValue::Failed(msg) => Some(msg.to_string()),
        PropertyValue::Secret(inner) => failure_of(inner),
        PropertyValue::Output(o) => o.value.as_deref().and_then(failure_of),
        PropertyValue::Array(items) => items.iter().find_map(failure_of),
        PropertyValue::Object(m) => m.values().find_map(failure_of),
        _ => None,
    }
}

async fn do_register(inner: Arc<ContextInner>, req: RegisterRequest) -> RegisterOutcome {
    let fail = |msg: String| RegisterOutcome {
        urn: String::new(),
        id: None,
        outputs: PropertyMap::new(),
        error: Some(msg),
        failed: None,
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
        None => match &req.options.provider_value {
            Some(v) => provider_ref_from_value(&v.data().await.value),
            None => String::new(),
        },
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
        // Depending on a resource that failed to register means this one
        // cannot be created either; propagate rather than racing the engine
        // to register something it will skip.
        if let Some(msg) = &dep.state.clone().await.failed {
            return propagated(msg.clone());
        }
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
    let mut failed_input: Option<String> = None;
    for (key, out) in req.inputs {
        // A deferred input would deadlock: the component supplying it is
        // waiting on this registration. Leave it out entirely.
        if req.deferred_inputs.contains(&key) {
            continue;
        }
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
        if let Some(msg) = failure_of(&data.value) {
            failed_input = Some(msg);
        }
        object.insert(key, encode_value(data, inner.features));
    }
    // An input carrying a failed resource's value means this resource can
    // never be created; `recover` intercepts the failure before it gets
    // here when the program handles it.
    if let Some(msg) = failed_input {
        return propagated(msg);
    }

    // Required inputs are checked against the marshalled map rather than
    // against the args struct, because `into_inputs` has already applied any
    // schema default — a required property that carries one is supplied, not
    // missing. Wire names are used deliberately: that is what the schema and
    // every other language's docs show.
    let missing: Vec<&str> = req
        .required
        .iter()
        .copied()
        .filter(|k| !object.contains_key(*k))
        .collect();
    if !missing.is_empty() {
        let msg = format!(
            "{} resource '{}': missing required {} {}",
            req.type_,
            req.name,
            if missing.len() == 1 {
                "input"
            } else {
                "inputs"
            },
            missing
                .iter()
                .map(|m| format!("`{m}`"))
                .collect::<Vec<_>>()
                .join(", "),
        );
        // Logged as well as returned: `finish` keeps only the first error, and
        // a program with three broken resources should report all three.
        if let Some(engine) = &inner.engine {
            let mut engine = engine.clone();
            let _ = engine
                .log(pulumirpc::LogRequest {
                    severity: pulumirpc::LogSeverity::Error as i32,
                    message: msg.clone(),
                    ..Default::default()
                })
                .await;
        }
        return fail(msg);
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
        custom_timeouts,
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
        package_ref: match &req.package {
            Some(p) => package_ref(&inner, p).await,
            None => String::new(),
        },
        hooks: if req.options.hooks.is_empty() {
            None
        } else {
            Some(req.options.hooks.to_proto())
        },
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
    // The engine reports a failed registration in-band because we advertised
    // supports_result_reporting; it sends no message, so synthesize one like
    // the other SDKs do.
    let failed = if response.result != pulumirpc::Result::Success as i32 {
        Some(format!(
            "resource {} [{}] failed to register",
            req.name, req.type_
        ))
    } else {
        None
    };
    RegisterOutcome {
        urn: response.urn,
        id: if req.custom { Some(response.id) } else { None },
        outputs,
        error: None,
        failed,
        unknown: response.unknown,
    }
}

// One argument per Read RPC field the caller can vary. Bundling them into a
// struct would only move the same eight values behind a name that no other
// call site would use.
#[allow(clippy::too_many_arguments)]
async fn do_read(
    inner: Arc<ContextInner>,
    type_: String,
    name: String,
    id: Output<PropertyValue>,
    inputs: Vec<(String, Output<PropertyValue>)>,
    version: String,
    provider: Option<Resource>,
    options: ResourceOptions,
) -> RegisterOutcome {
    let fail = |msg: String| RegisterOutcome {
        urn: String::new(),
        id: None,
        outputs: PropertyMap::new(),
        error: Some(msg),
        failed: None,
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
    // An explicit `depends_on` on a read has to reach the engine too; the
    // input values alone only cover the resources the read's own arguments
    // came from.
    for dep in &options.depends_on {
        dependencies.insert(await_urn(dep).await);
    }
    for (key, out) in inputs {
        let data = out.data().await;
        for d in &data.deps {
            dependencies.insert(d.clone());
        }
        properties.insert(key, encode_value(data, inner.features));
    }

    // A resource read through an explicit provider has to name it, or the
    // engine reads it with the default provider instead.
    let provider = match &provider {
        Some(p) => match p.provider_ref().data().await.value {
            PropertyValue::String(s) => s,
            _ => String::new(),
        },
        None => String::new(),
    };

    let request = pulumirpc::ReadResourceRequest {
        id: id_str.clone(),
        r#type: type_.clone(),
        name: name.clone(),
        parent,
        provider,
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
            return fail(format!(
                "reading resource {name} ({type_}): {}",
                e.message()
            ));
        }
    };
    let outputs = match &response.properties {
        Some(s) => unmarshal_properties(s),
        None => PropertyMap::new(),
    };
    RegisterOutcome {
        urn: response.urn,
        failed: None,
        id: Some(id_str),
        outputs,
        error: None,
        unknown: false,
    }
}

/// Perform a resource method call, returning the provider's return object.
async fn do_call(
    inner: Arc<ContextInner>,
    tok: String,
    self_: Resource,
    args: Vec<(String, Output<PropertyValue>)>,
) -> Result<OutputData> {
    // A call's result takes its secretness and its dependencies from the
    // response alone — the provider decides what the returned values depend
    // on and whether they are sensitive. The arguments' own secretness and
    // dependencies travel separately, in `arg_dependencies`, so the provider
    // can see them; folding them into the result as well would mark plain
    // return values secret and record dependencies the provider did not
    // claim. Go does the same (CallPackage keeps two separate `deps`), and so
    // does Python.
    //
    // `do_invoke` deliberately does the opposite, because Go's invoke path
    // does too. The asymmetry is in the reference implementation, not an
    // oversight here.
    let mut deps: Vec<String> = vec![];
    let mut arg_map = BTreeMap::new();
    let mut arg_dependencies = HashMap::new();
    for (key, out) in args {
        let data = out.data().await;
        arg_dependencies.insert(
            key.clone(),
            pulumirpc::resource_call_request::ArgumentDependencies {
                urns: data.deps.clone(),
            },
        );
        arg_map.insert(key, encode_value(data, inner.features));
    }

    // The receiver travels as a resource reference under __self__.
    let outcome = self_.state.clone().await;
    let self_ref = crate::value::ResourceReference {
        urn: outcome.urn.clone(),
        id: if self_.custom {
            Some(outcome.id.clone().filter(|i| !i.is_empty()))
        } else {
            None
        },
        package_version: self_.version.clone(),
    };
    arg_map.insert(
        "__self__".to_string(),
        PropertyValue::ResourceReference(self_ref),
    );
    if !outcome.urn.is_empty() {
        arg_dependencies.insert(
            "__self__".to_string(),
            pulumirpc::resource_call_request::ArgumentDependencies {
                urns: vec![outcome.urn.clone()],
            },
        );
    }

    let provider = match &self_.provider {
        Some(p) => match p.provider_ref().data().await.value {
            PropertyValue::String(s) => s,
            _ => String::new(),
        },
        None => String::new(),
    };

    let request = pulumirpc::ResourceCallRequest {
        tok: tok.clone(),
        args: Some(marshal_properties(&arg_map)),
        arg_dependencies,
        provider,
        version: self_.version.clone(),
        plugin_download_url: self_.plugin_download_url.clone(),
        accepts_byte_string: true,
        ..Default::default()
    };

    let mut monitor = inner.monitor.clone();
    let response = monitor
        .call(request)
        .await
        .map_err(|e| Error::new(format!("calling {}: {}", tok, e.message())))?
        .into_inner();
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
        return Err(Error::new(format!("calling {}: {}", tok, msgs.join("; "))));
    }

    for d in response.return_dependencies.values() {
        deps.extend(d.urns.iter().cloned());
    }
    let ret = match &response.r#return {
        Some(s) => PropertyValue::Object(unmarshal_properties(s)),
        None => PropertyValue::Object(BTreeMap::new()),
    };
    let data = OutputData::from_value(ret);
    Ok(OutputData {
        value: data.value,
        secret: data.secret,
        deps: deps.into_iter().chain(data.deps).collect(),
    })
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
    if !known || (inner.settings.dry_run && !deps.is_empty() && !inner.features.invoke_depends_on) {
        return Ok(OutputData {
            value: PropertyValue::Computed,
            secret,
            deps,
        });
    }

    // Which provider serves this invoke, the way Go's getProvider decides it:
    // start from what the parent carries for the invoke's package, and let an
    // explicit provider override it only when it serves that same package. A
    // provider for another package is not applicable and is discarded rather
    // than routing the invoke to the wrong plugin.
    //
    // The parent's map is where a singular `provider` option on that parent
    // ends up (see register_resource), so this is the path by which an invoke
    // inherits a provider that was never named in a providers map.
    let pkg = tok.split(':').next().unwrap_or_default().to_string();
    let mut chosen = opts
        .parent
        .as_ref()
        .and_then(|parent| parent.pulumi_providers().get(&pkg).cloned());
    if let Some(p) = &opts.provider {
        if p.package == pkg {
            chosen = Some(p.clone());
        }
    }
    let provider = match &chosen {
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
        package_ref: match &opts.package {
            Some(p) => package_ref(&inner, p).await,
            None => String::new(),
        },
        ..Default::default()
    };

    let mut monitor = inner.monitor.clone();
    let response = monitor
        .invoke(request)
        .await
        .map_err(|e| Error::new(format!("invoking {}: {}", tok, e.message())))?;
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
        return Ok(OutputData {
            value: PropertyValue::Computed,
            secret,
            deps,
        });
    }

    let ret = match &response.r#return {
        Some(s) => PropertyValue::Object(unmarshal_properties(s)),
        None => PropertyValue::Object(BTreeMap::new()),
    };
    let data = OutputData::from_value(ret);
    Ok(OutputData {
        value: data.value,
        // Unlike `do_call`, an invoke's result *does* inherit its arguments'
        // secretness — Go's invoke path does the same. `l2-invoke-secrets`
        // depends on it: an invoke given a secret argument must return a
        // secret result even when the provider does not mark it.
        secret: secret || data.secret,
        deps: deps.into_iter().chain(data.deps).collect(),
    })
}

/// Build a [`Struct`] from marshaled fields — exposed for the runtime module.
pub(crate) fn empty_struct() -> Struct {
    Struct {
        fields: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A context whose monitor channel is never connected. The required-input
    /// check runs before any RPC, so it can be exercised without an engine.
    fn offline_context() -> Arc<ContextInner> {
        let channel = Channel::from_static("http://127.0.0.1:1").connect_lazy();
        Arc::new(ContextInner {
            monitor: ResourceMonitorClient::new(channel),
            engine: None,
            settings: RunSettings::default(),
            features: Features::default(),
            config: Config::default(),
            stack_urn: tokio::sync::OnceCell::new(),
            callbacks: tokio::sync::OnceCell::new(),
            package_refs: tokio::sync::Mutex::new(HashMap::new()),
            hydrated: tokio::sync::Mutex::new(HashMap::new()),
            pending: Mutex::new(vec![]),
            exports: Mutex::new(vec![]),
        })
    }

    fn request(
        inputs: Vec<(&str, PropertyValue)>,
        required: &'static [&'static str],
    ) -> RegisterRequest {
        RegisterRequest {
            type_: "test:index:Thing".to_string(),
            name: "thing".to_string(),
            custom: true,
            remote: false,
            version: String::new(),
            plugin_download_url: String::new(),
            inputs: inputs
                .into_iter()
                .map(|(k, v)| (k.to_string(), Output::from_value(v)))
                .collect(),
            options: ResourceOptions::default(),
            package: None,
            deferred_inputs: vec![],
            required,
        }
    }

    #[tokio::test]
    async fn a_missing_required_input_fails_the_registration_by_name() {
        // Every generated args field is an Option, so this is the only place
        // a forgotten required input is caught before the provider sees it.
        let outcome = do_register(offline_context(), request(vec![], &["bucket"])).await;
        let err = outcome
            .error
            .expect("a missing required input was not reported");
        assert!(err.contains("test:index:Thing"), "no resource type: {err}");
        assert!(err.contains("thing"), "no resource name: {err}");
        assert!(
            err.contains("`bucket`"),
            "the missing input is not named: {err}"
        );
    }

    #[tokio::test]
    async fn every_missing_required_input_is_named_at_once() {
        let outcome = do_register(offline_context(), request(vec![], &["bucket", "key"])).await;
        let err = outcome.error.unwrap();
        assert!(err.contains("`bucket`") && err.contains("`key`"), "{err}");
        assert!(err.contains("inputs"), "plural not used: {err}");
    }

    /// Everything the monitor negotiates except first-class output values —
    /// the degrade path `encode_value` takes for older monitors.
    fn no_output_values() -> Features {
        Features {
            secrets: true,
            resource_references: true,
            output_values: false,
            invoke_depends_on: false,
        }
    }

    #[test]
    fn a_degraded_unknown_keeps_its_secretness() {
        // `Context::finish` wraps this exact case as Secret(Computed), so
        // dropping the flag here meant one and the same value marshaled
        // secret as a stack output and plain as a resource input.
        let data = OutputData {
            value: PropertyValue::Computed,
            secret: true,
            deps: vec![],
        };
        assert_eq!(
            encode_value(data, no_output_values()),
            PropertyValue::Secret(Box::new(PropertyValue::Computed)),
            "an unknown secret input was degraded to a plain unknown"
        );
    }

    #[test]
    fn degrading_reaches_output_values_nested_in_a_collection() {
        // `output::all`/`object` put the wrappers on the *elements* so that
        // partially-known collections keep their per-element flags. Stripping
        // only the top level sent an OUTPUT_VALUE_SIG object to a monitor
        // that never negotiated it, which reads back as a bare object with a
        // `4dabf18…` key.
        let element = PropertyValue::Output(crate::value::OutputValue {
            value: Some(Box::new(PropertyValue::String("n".into()))),
            secret: true,
            dependencies: vec!["urn:a".into()],
        });
        let data = OutputData {
            value: PropertyValue::Array(vec![element, PropertyValue::String("x".into())]),
            secret: false,
            deps: vec!["urn:a".into()],
        };
        assert_eq!(
            encode_value(data, no_output_values()),
            PropertyValue::Array(vec![
                PropertyValue::Secret(Box::new(PropertyValue::String("n".into()))),
                PropertyValue::String("x".into()),
            ]),
        );
    }

    #[test]
    fn output_values_survive_untouched_when_the_monitor_wants_them() {
        let data = OutputData {
            value: PropertyValue::String("n".into()),
            secret: false,
            deps: vec!["urn:a".into()],
        };
        let features = Features {
            output_values: true,
            ..no_output_values()
        };
        match encode_value(data, features) {
            PropertyValue::Output(o) => assert_eq!(o.dependencies, vec!["urn:a".to_string()]),
            other => panic!("expected an output value, got {other:?}"),
        }
    }

    #[test]
    fn a_provider_urn_yields_its_package() {
        assert_eq!(
            provider_package_of_urn("urn:pulumi:dev::proj::pulumi:providers:aws::prov"),
            "aws"
        );
        // Not a provider, so no package: an ordinary resource must never be
        // folded into the providers map.
        assert_eq!(
            provider_package_of_urn("urn:pulumi:dev::proj::aws:s3/bucket:Bucket::b"),
            ""
        );
        assert_eq!(provider_package_of_urn("nonsense"), "");
    }

    #[tokio::test]
    async fn an_input_supplied_by_a_schema_default_is_not_missing() {
        // `into_inputs` applies schema defaults before the map gets here, so
        // the check has to look at the produced inputs, not at the args
        // struct: a required property carrying a default is supplied.
        let outcome = do_register(
            offline_context(),
            request(
                vec![("bucket", PropertyValue::String("b".into()))],
                &["bucket"],
            ),
        )
        .await;
        // No missing-input error. The registration fails later, on the
        // unconnectable monitor, which is what proves the check passed.
        let err = outcome.error.unwrap_or_default();
        assert!(!err.contains("missing required"), "{err}");
    }
}

/// What actually reaches the monitor when providers are involved.
///
/// The engine masks nearly all of this: `inheritFromParent` copies a
/// parent's provider onto a custom child, and the monitor falls back to the
/// receiver's goal state for `Call`. A language can therefore get every one
/// of these wrong and still pass the conformance suite, which is exactly why
/// they are pinned here instead.
#[cfg(test)]
mod provider_tests {
    use super::*;
    use crate::monitor_test_support::fake_monitor_context;

    /// Register a provider resource and hand back a handle to it.
    fn provider(ctx: &Context, package: &str, name: &str) -> Resource {
        ctx.register_resource(RegisterRequest {
            type_: format!("pulumi:providers:{package}"),
            name: name.to_string(),
            custom: true,
            ..Default::default()
        })
    }

    fn register(ctx: &Context, type_: &str, name: &str, options: ResourceOptions) -> Resource {
        ctx.register_resource(RegisterRequest {
            type_: type_.to_string(),
            name: name.to_string(),
            custom: true,
            options,
            ..Default::default()
        })
    }

    /// The wire `provider` of the registration for `name`.
    fn wire_provider(
        captured: &std::sync::Mutex<crate::monitor_test_support::Captured>,
        name: &str,
    ) -> String {
        captured
            .lock()
            .unwrap()
            .registers
            .iter()
            .find(|r| r.name == name)
            .unwrap_or_else(|| panic!("no registration for {name}"))
            .provider
            .clone()
    }

    #[tokio::test]
    async fn a_child_inherits_its_parents_singular_provider() {
        let (ctx, captured) = fake_monitor_context().await;
        let prov = provider(&ctx, "simple", "prov");
        let parent = register(
            &ctx,
            "simple:index:Resource",
            "parent1",
            ResourceOptions {
                provider: Some(prov.clone()),
                ..Default::default()
            },
        );
        let _child = register(
            &ctx,
            "simple:index:Resource",
            "child1",
            ResourceOptions {
                parent: Some(parent),
                ..Default::default()
            },
        );
        ctx.drain().await.unwrap();
        assert!(
            !wire_provider(&captured, "child1").is_empty(),
            "the child did not inherit its parent's provider"
        );
    }

    #[tokio::test]
    async fn a_provider_for_another_package_is_discarded() {
        // Sending it would route the resource to the wrong plugin.
        let (ctx, captured) = fake_monitor_context().await;
        let prov = provider(&ctx, "simple", "prov");
        let _res = register(
            &ctx,
            "primitive:index:Resource",
            "mismatch",
            ResourceOptions {
                provider: Some(prov),
                ..Default::default()
            },
        );
        ctx.drain().await.unwrap();
        assert_eq!(wire_provider(&captured, "mismatch"), "");
    }

    #[tokio::test]
    async fn a_provider_for_the_matching_package_is_sent() {
        let (ctx, captured) = fake_monitor_context().await;
        let prov = provider(&ctx, "simple", "prov");
        let _res = register(
            &ctx,
            "simple:index:Resource",
            "matched",
            ResourceOptions {
                provider: Some(prov),
                ..Default::default()
            },
        );
        ctx.drain().await.unwrap();
        assert!(!wire_provider(&captured, "matched").is_empty());
    }

    #[tokio::test]
    async fn a_childs_own_provider_overrides_one_inherited_from_its_parent() {
        // The conflict test is against the *explicit* providers map, not the
        // merged one — testing against the merged map would skip this insert
        // and leave the child on its parent's provider.
        let (ctx, captured) = fake_monitor_context().await;
        let prov_a = provider(&ctx, "simple", "provA");
        let prov_b = provider(&ctx, "simple", "provB");
        let parent = register(
            &ctx,
            "simple:index:Resource",
            "parent",
            ResourceOptions {
                provider: Some(prov_a),
                ..Default::default()
            },
        );
        let child = register(
            &ctx,
            "simple:index:Resource",
            "child",
            ResourceOptions {
                parent: Some(parent),
                provider: Some(prov_b.clone()),
                ..Default::default()
            },
        );
        // The grandchild is where the two merge rules differ. The child's own
        // wire provider is provB either way, because getProvider prefers the
        // singular option; what changes is the map the child hands down. A
        // conflict test against the merged map would leave provA in it.
        let _grandchild = register(
            &ctx,
            "simple:index:Resource",
            "grandchild",
            ResourceOptions {
                parent: Some(child),
                ..Default::default()
            },
        );
        ctx.drain().await.unwrap();
        let want = match prov_b.provider_ref().data().await.value {
            PropertyValue::String(s) => s,
            other => panic!("provider ref is not a string: {other:?}"),
        };
        assert_eq!(wire_provider(&captured, "child"), want);
        assert_eq!(
            wire_provider(&captured, "grandchild"),
            want,
            "the grandchild inherited the parent's provider instead of the child's"
        );
    }

    #[tokio::test]
    async fn a_read_is_read_through_its_provider() {
        // do_read used to send no provider at all, so a resource read through
        // an explicit provider was read by the default one instead.
        let (ctx, captured) = fake_monitor_context().await;
        let prov = provider(&ctx, "simple", "prov");
        let _read = ctx.read_resource(
            "simple:index:Resource",
            "imported",
            Output::from_value(PropertyValue::String("id-0".into())),
            vec![],
            "",
            ResourceOptions {
                provider: Some(prov),
                ..Default::default()
            },
        );
        ctx.drain().await.unwrap();
        let reads = captured.lock().unwrap();
        let read = reads.reads.first().expect("no read reached the monitor");
        assert!(
            !read.provider.is_empty(),
            "the read did not name its provider"
        );
    }

    #[tokio::test]
    async fn an_invoke_result_inherits_its_arguments_secretness() {
        // The fake monitor returns a plain, non-secret result, so the only
        // way the output can be secret is the argument. Go's invoke path does
        // this, and l2-invoke-secrets depends on it.
        let (ctx, _captured) = fake_monitor_context().await;
        let out = ctx.invoke(
            "simple-invoke:index:secretInvoke",
            vec![(
                "value".to_string(),
                crate::pv::secret(crate::pv::string("goodbye")),
            )],
            InvokeOptions::default(),
        );
        assert!(
            out.data().await.secret,
            "an invoke dropped its argument's secretness"
        );
    }

    #[tokio::test]
    async fn a_call_result_does_not_inherit_its_arguments_secretness() {
        // A call is the other way round: the provider decides what its return
        // value is, and marking it secret because an argument was would put
        // values in the state file that the provider never called sensitive.
        // The argument's secretness still reaches the provider through
        // arg_dependencies.
        let (ctx, captured) = fake_monitor_context().await;
        let receiver = register(
            &ctx,
            "simple:index:Resource",
            "res",
            ResourceOptions::default(),
        );
        let out = ctx.call(
            "simple:index:Resource/method",
            &receiver,
            vec![(
                "value".to_string(),
                crate::pv::secret(crate::pv::string("shh")),
            )],
        );
        assert!(
            !out.data().await.secret,
            "a call result took its secretness from an argument"
        );
        let calls = captured.lock().unwrap();
        let call = calls.calls.first().expect("no call reached the monitor");
        assert!(
            call.arg_dependencies.contains_key("__self__"),
            "the receiver was not declared as an argument dependency"
        );
    }

    #[tokio::test]
    async fn an_invoke_parented_to_a_resource_uses_that_resources_provider() {
        // This is the one case the engine does not mask: the monitor resolves
        // an invoke's provider from what the program sends, and a component's
        // provider map, but never from a custom parent's singular provider.
        let (ctx, captured) = fake_monitor_context().await;
        let prov = provider(&ctx, "simple", "prov");
        let parent = register(
            &ctx,
            "simple:index:Resource",
            "parent",
            ResourceOptions {
                provider: Some(prov),
                ..Default::default()
            },
        );
        let _ = ctx
            .invoke(
                "simple:index:myInvoke",
                vec![],
                InvokeOptions {
                    parent: Some(parent),
                    ..Default::default()
                },
            )
            .data()
            .await;
        let invokes = captured.lock().unwrap();
        let invoke = invokes
            .invokes
            .first()
            .expect("no invoke reached the monitor");
        assert!(
            !invoke.provider.is_empty(),
            "the invoke did not inherit its parent's provider"
        );
    }
}
