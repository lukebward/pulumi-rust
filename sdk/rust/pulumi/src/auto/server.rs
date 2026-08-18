//! The in-process language host behind inline programs.
//!
//! When a workspace holds a Rust closure as its program, stack operations
//! pass `--client=127.0.0.1:<port>` and the engine talks to this server
//! instead of launching a language plugin. Only the handful of methods the
//! engine needs for that path do real work; everything else answers
//! `Unimplemented`, which the engine treats as "nothing to report" — the
//! same shape as the Go SDK's inline server.

use std::collections::HashMap;

use tonic::{Request, Response, Status};

use super::errors::{Error, Result};
use super::ProgramFn;
use crate::context::RunSettings;
use crate::pulumirpc;
use crate::pulumirpc::language_runtime_server::{LanguageRuntime, LanguageRuntimeServer};

/// Matches the engine's 400MiB gRPC message cap.
const MAX_RPC_MESSAGE_SIZE: usize = 1024 * 1024 * 400;

/// A running inline language server: an address to hand the CLI, and the
/// means to stop serving once the CLI has exited.
pub(crate) struct LanguageServer {
    address: String,
    shutdown: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<std::result::Result<(), tonic::transport::Error>>,
}

impl LanguageServer {
    /// Bind an ephemeral local port and serve `program` from it.
    pub(crate) async fn start(program: ProgramFn) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| Error::setup(format!("failed to bind language server: {e}")))?;
        let address = listener
            .local_addr()
            .map(|a| a.to_string())
            .map_err(|e| Error::setup(format!("failed to read language server address: {e}")))?;

        let (shutdown, rx) = tokio::sync::oneshot::channel::<()>();
        let service = LanguageRuntimeServer::new(InlineLanguageHost { program })
            .max_decoding_message_size(MAX_RPC_MESSAGE_SIZE)
            .max_encoding_message_size(MAX_RPC_MESSAGE_SIZE);
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        let task = tokio::spawn(
            tonic::transport::Server::builder()
                .add_service(service)
                .serve_with_incoming_shutdown(incoming, async {
                    let _ = rx.await;
                }),
        );
        Ok(LanguageServer {
            address,
            shutdown,
            task,
        })
    }

    /// The `host:port` the CLI's `--client` flag takes; no scheme.
    pub(crate) fn address(&self) -> &str {
        &self.address
    }

    /// Stop serving. Graceful: an in-flight `Run` finishes first, though in
    /// practice the CLI has already exited by the time this is called.
    pub(crate) async fn close(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

struct InlineLanguageHost {
    program: ProgramFn,
}

fn unimplemented<T>(what: &str) -> std::result::Result<Response<T>, Status> {
    Err(Status::unimplemented(format!(
        "{what} is not implemented by the automation-API language host"
    )))
}

#[tonic::async_trait]
impl LanguageRuntime for InlineLanguageHost {
    async fn run(
        &self,
        request: Request<pulumirpc::RunRequest>,
    ) -> std::result::Result<Response<pulumirpc::RunResponse>, Status> {
        let req = request.into_inner();
        let settings = RunSettings {
            project: req.project,
            stack: req.stack,
            // Mirror the SDK's env-var path when the engine sends none.
            organization: if req.organization.is_empty() {
                "organization".to_string()
            } else {
                req.organization
            },
            dry_run: req.dry_run,
            monitor_addr: req.monitor_address,
            // With no handshake, the engine's own address arrives as the
            // first program argument.
            engine_addr: req.args.first().cloned().unwrap_or_default(),
            config: req.config.into_iter().collect::<HashMap<_, _>>(),
            config_secret_keys: req.config_secret_keys,
        };
        // The program's failure travels in-band; a gRPC error would read
        // as the language host itself breaking.
        let error = match crate::runtime::run_inline(settings, self.program.clone()).await {
            Ok(()) => String::new(),
            Err(e) => e.to_string(),
        };
        Ok(Response::new(pulumirpc::RunResponse { error, bail: false }))
    }

    async fn get_required_plugins(
        &self,
        _request: Request<pulumirpc::GetRequiredPluginsRequest>,
    ) -> std::result::Result<Response<pulumirpc::GetRequiredPluginsResponse>, Status> {
        // The engine installs plugins on demand for inline programs.
        Ok(Response::new(Default::default()))
    }

    async fn get_required_packages(
        &self,
        _request: Request<pulumirpc::GetRequiredPackagesRequest>,
    ) -> std::result::Result<Response<pulumirpc::GetRequiredPackagesResponse>, Status> {
        Ok(Response::new(Default::default()))
    }

    async fn get_plugin_info(
        &self,
        _request: Request<()>,
    ) -> std::result::Result<Response<pulumirpc::PluginInfo>, Status> {
        Ok(Response::new(pulumirpc::PluginInfo {
            version: "1.0.0".to_string(),
        }))
    }

    type InstallDependenciesStream =
        futures::stream::Empty<std::result::Result<pulumirpc::InstallDependenciesResponse, Status>>;

    async fn install_dependencies(
        &self,
        _request: Request<pulumirpc::InstallDependenciesRequest>,
    ) -> std::result::Result<Response<Self::InstallDependenciesStream>, Status> {
        // An inline program's dependencies are this process's; nothing to do.
        Ok(Response::new(futures::stream::empty()))
    }

    async fn cancel(&self, _request: Request<()>) -> std::result::Result<Response<()>, Status> {
        Ok(Response::new(()))
    }

    async fn handshake(
        &self,
        _request: Request<pulumirpc::LanguageHandshakeRequest>,
    ) -> std::result::Result<Response<pulumirpc::LanguageHandshakeResponse>, Status> {
        // Deliberately unimplemented: without a handshake the engine falls
        // back to passing its address through Run's arguments, which is
        // the contract `run` above relies on.
        unimplemented("Handshake")
    }

    async fn runtime_options_prompts(
        &self,
        _request: Request<pulumirpc::RuntimeOptionsRequest>,
    ) -> std::result::Result<Response<pulumirpc::RuntimeOptionsResponse>, Status> {
        unimplemented("RuntimeOptionsPrompts")
    }

    async fn template(
        &self,
        _request: Request<pulumirpc::TemplateRequest>,
    ) -> std::result::Result<Response<pulumirpc::TemplateResponse>, Status> {
        unimplemented("Template")
    }

    async fn about(
        &self,
        _request: Request<pulumirpc::AboutRequest>,
    ) -> std::result::Result<Response<pulumirpc::AboutResponse>, Status> {
        unimplemented("About")
    }

    async fn get_program_dependencies(
        &self,
        _request: Request<pulumirpc::GetProgramDependenciesRequest>,
    ) -> std::result::Result<Response<pulumirpc::GetProgramDependenciesResponse>, Status> {
        unimplemented("GetProgramDependencies")
    }

    type RunPluginStream =
        futures::stream::Empty<std::result::Result<pulumirpc::RunPluginResponse, Status>>;

    async fn run_plugin(
        &self,
        _request: Request<pulumirpc::RunPluginRequest>,
    ) -> std::result::Result<Response<Self::RunPluginStream>, Status> {
        unimplemented("RunPlugin")
    }

    async fn generate_program(
        &self,
        _request: Request<pulumirpc::GenerateProgramRequest>,
    ) -> std::result::Result<Response<pulumirpc::GenerateProgramResponse>, Status> {
        unimplemented("GenerateProgram")
    }

    async fn generate_project(
        &self,
        _request: Request<pulumirpc::GenerateProjectRequest>,
    ) -> std::result::Result<Response<pulumirpc::GenerateProjectResponse>, Status> {
        unimplemented("GenerateProject")
    }

    async fn generate_package(
        &self,
        _request: Request<pulumirpc::GeneratePackageRequest>,
    ) -> std::result::Result<Response<pulumirpc::GeneratePackageResponse>, Status> {
        unimplemented("GeneratePackage")
    }

    async fn pack(
        &self,
        _request: Request<pulumirpc::PackRequest>,
    ) -> std::result::Result<Response<pulumirpc::PackResponse>, Status> {
        unimplemented("Pack")
    }

    async fn link(
        &self,
        _request: Request<pulumirpc::LinkRequest>,
    ) -> std::result::Result<Response<pulumirpc::LinkResponse>, Status> {
        unimplemented("Link")
    }
}
