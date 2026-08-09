//! Hosting a component provider written in Rust.
//!
//! The engine launches a provider as a plugin, hands it an engine address on
//! the command line, and expects the plugin's gRPC port on the first line of
//! stdout. This module serves the `ResourceProvider` service for a provider
//! whose only job is to construct component resources: it answers `GetSchema`
//! with the package schema and `Construct` by running the component body
//! against the engine's resource monitor.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use tonic::{Request, Response, Status};

use crate::context::{
    Alias, AliasParent, AliasSpec, Context, CustomTimeouts, ResourceOptions, RunSettings,
};
use crate::error::{Error, Result};
use crate::hooks::{ResourceHook, ResourceHookBinding};
use crate::output::Output;
use crate::pulumirpc;
use crate::pulumirpc::resource_provider_server::{ResourceProvider, ResourceProviderServer};
use crate::value::{marshal_properties, unmarshal_properties, PropertyMap, PropertyValue};

/// What a component's constructor receives.
pub struct ConstructArgs {
    /// A context wired to the engine's monitor for this construction.
    pub ctx: Context,
    /// The component's type token.
    pub type_: String,
    /// The component's name.
    pub name: String,
    /// The component's inputs.
    pub inputs: PropertyMap,
    /// Parent and providers to pass to the component registration.
    pub options: ResourceOptions,
}

/// What a component's constructor returns.
pub struct ConstructResult {
    /// The URN of the component that was registered.
    pub urn: String,
    /// The component's output properties.
    pub state: PropertyMap,
}

type ConstructFn =
    Arc<dyn Fn(ConstructArgs) -> BoxFuture<'static, Result<ConstructResult>> + Send + Sync>;

/// How to host a component provider.
pub struct ComponentProviderOptions {
    /// The package name, e.g. "conformance-component".
    pub name: String,
    /// The package version.
    pub version: String,
    /// The package schema, as JSON. The conformance loader binds programs
    /// against this schema and generates SDKs from it.
    pub schema: String,
    /// Constructs one component, dispatching on the type token.
    pub construct: ConstructFn,
}

struct Service {
    opts: Arc<ComponentProviderOptions>,
    /// The engine's address, so a component body that fails can report why.
    engine: Arc<Mutex<String>>,
}

#[tonic::async_trait]
impl ResourceProvider for Service {
    async fn get_schema(
        &self,
        _request: Request<pulumirpc::GetSchemaRequest>,
    ) -> std::result::Result<Response<pulumirpc::GetSchemaResponse>, Status> {
        Ok(Response::new(pulumirpc::GetSchemaResponse {
            schema: self.opts.schema.clone(),
        }))
    }

    async fn get_plugin_info(
        &self,
        _request: Request<()>,
    ) -> std::result::Result<Response<pulumirpc::PluginInfo>, Status> {
        Ok(Response::new(pulumirpc::PluginInfo {
            version: self.opts.version.clone(),
        }))
    }

    async fn check_config(
        &self,
        request: Request<pulumirpc::CheckRequest>,
    ) -> std::result::Result<Response<pulumirpc::CheckResponse>, Status> {
        // A component provider has no configuration of its own; echo the
        // news back so the engine records them unchanged.
        let request = request.into_inner();
        Ok(Response::new(pulumirpc::CheckResponse {
            inputs: request.news,
            failures: vec![],
        }))
    }

    async fn diff_config(
        &self,
        _request: Request<pulumirpc::DiffRequest>,
    ) -> std::result::Result<Response<pulumirpc::DiffResponse>, Status> {
        Ok(Response::new(pulumirpc::DiffResponse::default()))
    }

    async fn configure(
        &self,
        _request: Request<pulumirpc::ConfigureRequest>,
    ) -> std::result::Result<Response<pulumirpc::ConfigureResponse>, Status> {
        Ok(Response::new(pulumirpc::ConfigureResponse {
            accept_secrets: true,
            accept_resources: true,
            accept_outputs: true,
            supports_preview: true,
            ..Default::default()
        }))
    }

    async fn construct(
        &self,
        request: Request<pulumirpc::ConstructRequest>,
    ) -> std::result::Result<Response<pulumirpc::ConstructResponse>, Status> {
        let request = request.into_inner();
        let opts = self.opts.clone();
        let engine = self.engine.lock().unwrap().clone();
        construct(opts, engine, request)
            .await
            .map(Response::new)
            .map_err(|e| Status::internal(format!("{e}")))
    }

    // A component provider serves none of the custom-resource lifecycle;
    // the engine never calls these for a component.
    async fn handshake(
        &self,
        _request: Request<pulumirpc::ProviderHandshakeRequest>,
    ) -> std::result::Result<Response<pulumirpc::ProviderHandshakeResponse>, Status> {
        // Handshake is where capabilities are negotiated; the engine records
        // these answers and never revisits them, so they must match what
        // `configure` reports. In particular a provider that does not claim
        // secrets support here is refused Construct outright.
        Ok(Response::new(pulumirpc::ProviderHandshakeResponse {
            accept_secrets: true,
            accept_resources: true,
            accept_outputs: true,
            ..Default::default()
        }))
    }

    async fn parameterize(
        &self,
        _request: Request<pulumirpc::ParameterizeRequest>,
    ) -> std::result::Result<Response<pulumirpc::ParameterizeResponse>, Status> {
        Err(Status::unimplemented("parameterize"))
    }

    async fn invoke(
        &self,
        _request: Request<pulumirpc::InvokeRequest>,
    ) -> std::result::Result<Response<pulumirpc::InvokeResponse>, Status> {
        Err(Status::unimplemented("invoke"))
    }

    async fn call(
        &self,
        _request: Request<pulumirpc::CallRequest>,
    ) -> std::result::Result<Response<pulumirpc::CallResponse>, Status> {
        Err(Status::unimplemented("call"))
    }

    async fn check(
        &self,
        request: Request<pulumirpc::CheckRequest>,
    ) -> std::result::Result<Response<pulumirpc::CheckResponse>, Status> {
        let request = request.into_inner();
        Ok(Response::new(pulumirpc::CheckResponse {
            inputs: request.news,
            failures: vec![],
        }))
    }

    async fn diff(
        &self,
        _request: Request<pulumirpc::DiffRequest>,
    ) -> std::result::Result<Response<pulumirpc::DiffResponse>, Status> {
        Ok(Response::new(pulumirpc::DiffResponse::default()))
    }

    async fn create(
        &self,
        _request: Request<pulumirpc::CreateRequest>,
    ) -> std::result::Result<Response<pulumirpc::CreateResponse>, Status> {
        Err(Status::unimplemented("create"))
    }

    async fn read(
        &self,
        _request: Request<pulumirpc::ReadRequest>,
    ) -> std::result::Result<Response<pulumirpc::ReadResponse>, Status> {
        Err(Status::unimplemented("read"))
    }

    type ListStream = futures::stream::Empty<std::result::Result<pulumirpc::ListResponse, Status>>;

    async fn list(
        &self,
        _request: Request<pulumirpc::ListRequest>,
    ) -> std::result::Result<Response<Self::ListStream>, Status> {
        Err(Status::unimplemented("list"))
    }

    async fn update(
        &self,
        _request: Request<pulumirpc::UpdateRequest>,
    ) -> std::result::Result<Response<pulumirpc::UpdateResponse>, Status> {
        Err(Status::unimplemented("update"))
    }

    async fn delete(
        &self,
        _request: Request<pulumirpc::DeleteRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        Err(Status::unimplemented("delete"))
    }

    async fn get_mapping(
        &self,
        _request: Request<pulumirpc::GetMappingRequest>,
    ) -> std::result::Result<Response<pulumirpc::GetMappingResponse>, Status> {
        Ok(Response::new(pulumirpc::GetMappingResponse::default()))
    }

    async fn get_mappings(
        &self,
        _request: Request<pulumirpc::GetMappingsRequest>,
    ) -> std::result::Result<Response<pulumirpc::GetMappingsResponse>, Status> {
        Ok(Response::new(pulumirpc::GetMappingsResponse::default()))
    }

    async fn cancel(
        &self,
        _request: Request<()>,
    ) -> std::result::Result<Response<()>, Status> {
        Ok(Response::new(()))
    }

    async fn attach(
        &self,
        _request: Request<pulumirpc::PluginAttach>,
    ) -> std::result::Result<Response<()>, Status> {
        Ok(Response::new(()))
    }
}

/// Rebuild the resource options the engine sent alongside a `Construct` call.
///
/// A remote component is registered twice: once by the program, which is where
/// the user's options are written down, and once by this provider against the
/// engine's monitor, which is the registration the engine actually records.
/// The engine forwards the program's options here so that the second
/// registration can carry them; anything not copied across is silently lost,
/// which is how an alias or an ignoreChanges on a remote component disappears.
fn construct_options(ctx: &Context, request: &pulumirpc::ConstructRequest) -> ResourceOptions {
    let mut options = ResourceOptions {
        protect: request.protect,
        additional_secret_outputs: request.additional_secret_outputs.clone(),
        ignore_changes: request.ignore_changes.clone(),
        delete_before_replace: request.delete_before_replace,
        retain_on_delete: request.retain_on_delete,
        replace_on_changes: request.replace_on_changes.clone(),
        ..Default::default()
    };

    if !request.parent.is_empty() {
        options.parent = Some(ctx.resource_from_urn(&request.parent));
    }
    for (pkg, reference) in &request.providers {
        options.providers.push((pkg.clone(), ctx.provider_from_reference(reference)));
    }
    for urn in &request.dependencies {
        options.depends_on.push(ctx.resource_from_urn(urn));
    }
    for urn in &request.replace_with {
        options.replace_with.push(ctx.resource_from_urn(urn));
    }
    if !request.deleted_with.is_empty() {
        options.deleted_with = Some(ctx.resource_from_urn(&request.deleted_with));
    }

    for alias in &request.aliases {
        use pulumirpc::alias::{spec, Alias as ProtoAlias};
        match &alias.alias {
            Some(ProtoAlias::Urn(urn)) => options.aliases.push(Alias::Urn(urn.clone())),
            Some(ProtoAlias::Spec(s)) => {
                // Empty strings mean "unset", i.e. inherit the resource's
                // current value, which is what `None` means to AliasSpec.
                let some = |v: &str| (!v.is_empty()).then(|| v.to_string());
                options.aliases.push(Alias::Spec(AliasSpec {
                    name: some(&s.name),
                    type_: some(&s.r#type),
                    stack: some(&s.stack),
                    project: some(&s.project),
                    parent: match &s.parent {
                        Some(spec::Parent::ParentUrn(urn)) => {
                            Some(AliasParent::Urn(ctx.resource_from_urn(urn)))
                        }
                        // `noParent: false` carries no information.
                        Some(spec::Parent::NoParent(true)) => Some(AliasParent::None),
                        _ => None,
                    },
                }));
            }
            None => {}
        }
    }

    if let Some(t) = &request.custom_timeouts {
        let timeout = |v: &str| {
            (!v.is_empty()).then(|| Output::from_value(PropertyValue::String(v.to_string())))
        };
        options.custom_timeouts = Some(CustomTimeouts {
            create: timeout(&t.create),
            update: timeout(&t.update),
            delete: timeout(&t.delete),
            read: timeout(&t.read),
        });
    }

    if let Some(value) = &request.replacement_trigger {
        let value = PropertyValue::from_proto(value);
        // A null trigger means the program explicitly left it unset.
        if !matches!(value, PropertyValue::Null) {
            options.replacement_trigger = Some(Output::from_value(value));
        }
    }

    if let Some(binding) = &request.resource_hooks {
        // The hooks themselves live in the program that registered them; the
        // engine only needs their names to bind them again here.
        let hooks = |names: &[String]| -> Vec<ResourceHook> {
            names.iter().map(|name| ResourceHook { name: name.clone() }).collect()
        };
        options.hooks = ResourceHookBinding {
            before_create: hooks(&binding.before_create),
            after_create: hooks(&binding.after_create),
            before_update: hooks(&binding.before_update),
            after_update: hooks(&binding.after_update),
            before_delete: hooks(&binding.before_delete),
            after_delete: hooks(&binding.after_delete),
            on_error: hooks(&binding.on_error),
        };
    }

    options
}

async fn construct(
    opts: Arc<ComponentProviderOptions>,
    engine_addr: String,
    request: pulumirpc::ConstructRequest,
) -> Result<pulumirpc::ConstructResponse> {
    let settings = RunSettings {
        project: request.project.clone(),
        stack: request.stack.clone(),
        organization: if request.organization.is_empty() {
            "organization".to_string()
        } else {
            request.organization.clone()
        },
        dry_run: request.dry_run,
        monitor_addr: request.monitor_endpoint.clone(),
        engine_addr,
        config: request.config.clone().into_iter().collect::<HashMap<_, _>>(),
        config_secret_keys: request.config_secret_keys.clone(),
    };
    let ctx = crate::runtime::connect_context(settings).await?;

    let inputs = match &request.inputs {
        Some(s) => unmarshal_properties(s),
        None => PropertyMap::new(),
    };

    let options = construct_options(&ctx, &request);

    let result = (opts.construct)(ConstructArgs {
        ctx: ctx.clone(),
        type_: request.r#type.clone(),
        name: request.name.clone(),
        inputs,
        options,
    })
    .await?;

    // Children register concurrently; make sure they finish before the
    // engine is told the component is done.
    ctx.drain().await?;

    Ok(pulumirpc::ConstructResponse {
        urn: result.urn,
        state: Some(marshal_properties(&result.state)),
        state_dependencies: HashMap::new(),
    })
}

/// Serve a component provider until the engine disconnects.
///
/// The engine passes its own address as the first argument and reads our
/// port from the first line of stdout, so nothing else may be printed there.
pub async fn component_provider_host(opts: ComponentProviderOptions) -> Result<()> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| Error::new(format!("binding provider server: {e}")))?;
    let port = listener
        .local_addr()
        .map_err(|e| Error::new(format!("reading provider address: {e}")))?
        .port();

    // The handshake: the engine reads this line and nothing before it.
    println!("{port}");
    use std::io::Write;
    let _ = std::io::stdout().flush();

    // The engine passes its address as the first argument; the handshake
    // supplies it too on newer engines.
    let engine = std::env::args().nth(1).unwrap_or_default();
    let service = Service { opts: Arc::new(opts), engine: Arc::new(Mutex::new(engine)) };
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    tonic::transport::Server::builder()
        .add_service(ResourceProviderServer::new(service))
        .serve_with_incoming(incoming)
        .await
        .map_err(|e| Error::new(format!("serving provider: {e}")))?;
    Ok(())
}
