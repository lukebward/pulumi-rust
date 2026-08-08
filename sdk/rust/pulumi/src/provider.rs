//! Hosting a component provider written in Rust.
//!
//! The engine launches a provider as a plugin, hands it an engine address on
//! the command line, and expects the plugin's gRPC port on the first line of
//! stdout. This module serves the `ResourceProvider` service for a provider
//! whose only job is to construct component resources: it answers `GetSchema`
//! with the package schema and `Construct` by running the component body
//! against the engine's resource monitor.

use std::collections::HashMap;
use std::sync::Arc;

use futures::future::BoxFuture;
use tonic::{Request, Response, Status};

use crate::context::{Context, ResourceOptions, RunSettings};
use crate::error::{Error, Result};
use crate::pulumirpc;
use crate::pulumirpc::resource_provider_server::{ResourceProvider, ResourceProviderServer};
use crate::value::{marshal_properties, unmarshal_properties, PropertyMap};

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
        construct(opts, request)
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
        Ok(Response::new(pulumirpc::ProviderHandshakeResponse::default()))
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

async fn construct(
    opts: Arc<ComponentProviderOptions>,
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
        engine_addr: String::new(),
        config: request.config.clone().into_iter().collect::<HashMap<_, _>>(),
        config_secret_keys: request.config_secret_keys.clone(),
    };
    let ctx = crate::runtime::connect_context(settings).await?;

    let inputs = match &request.inputs {
        Some(s) => unmarshal_properties(s),
        None => PropertyMap::new(),
    };

    // The engine tells us the parent and the providers the component and its
    // children should use.
    let mut options = ResourceOptions::default();
    if !request.parent.is_empty() {
        options.parent = Some(ctx.resource_from_urn(&request.parent));
    }
    for (pkg, reference) in &request.providers {
        options.providers.push((pkg.clone(), ctx.provider_from_reference(reference)));
    }

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

    let service = Service { opts: Arc::new(opts) };
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    tonic::transport::Server::builder()
        .add_service(ResourceProviderServer::new(service))
        .serve_with_incoming(incoming)
        .await
        .map_err(|e| Error::new(format!("serving provider: {e}")))?;
    Ok(())
}
