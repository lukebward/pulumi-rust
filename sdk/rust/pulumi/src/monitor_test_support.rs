//! A fake resource monitor, for testing the parts of the protocol the
//! conformance suite cannot see.
//!
//! The engine masks most provider-resolution mistakes: `inheritFromParent`
//! copies a parent's provider onto a custom child, and the monitor falls
//! back to the receiver's goal state for `Call`. So a language can get the
//! client half wrong and still pass `l2-resource-provider-inheritance`. The
//! only way to pin what we actually put on the wire is to be the monitor.

#![cfg(test)]

use std::sync::{Arc, Mutex};

use tonic::{Request, Response, Status};

use crate::pulumirpc;

/// Every request the fake monitor received, in arrival order.
#[derive(Default)]
pub(crate) struct Captured {
    pub registers: Vec<pulumirpc::RegisterResourceRequest>,
    pub reads: Vec<pulumirpc::ReadResourceRequest>,
    pub invokes: Vec<pulumirpc::ResourceInvokeRequest>,
    pub calls: Vec<pulumirpc::ResourceCallRequest>,
}

#[derive(Clone)]
pub(crate) struct FakeMonitor {
    pub captured: Arc<Mutex<Captured>>,
}

#[tonic::async_trait]
impl pulumirpc::resource_monitor_server::ResourceMonitor for FakeMonitor {
    async fn register_resource(
        &self,
        request: Request<pulumirpc::RegisterResourceRequest>,
    ) -> Result<Response<pulumirpc::RegisterResourceResponse>, Status> {
        let req = request.into_inner();
        // A URN the SDK can parse back into a package, so a provider
        // registered through this monitor behaves like a real one.
        let urn = format!("urn:pulumi:dev::proj::{}::{}", req.r#type, req.name);
        self.captured.lock().unwrap().registers.push(req);
        Ok(Response::new(pulumirpc::RegisterResourceResponse {
            urn,
            id: "id-1".to_string(),
            ..Default::default()
        }))
    }

    async fn read_resource(
        &self,
        request: Request<pulumirpc::ReadResourceRequest>,
    ) -> Result<Response<pulumirpc::ReadResourceResponse>, Status> {
        let req = request.into_inner();
        let urn = format!("urn:pulumi:dev::proj::{}::{}", req.r#type, req.name);
        self.captured.lock().unwrap().reads.push(req);
        Ok(Response::new(pulumirpc::ReadResourceResponse { urn, properties: None }))
    }

    async fn invoke(
        &self,
        request: Request<pulumirpc::ResourceInvokeRequest>,
    ) -> Result<Response<pulumirpc::ResourceInvokeResponse>, Status> {
        self.captured.lock().unwrap().invokes.push(request.into_inner());
        Ok(Response::new(pulumirpc::ResourceInvokeResponse::default()))
    }

    async fn call(
        &self,
        request: Request<pulumirpc::ResourceCallRequest>,
    ) -> Result<Response<pulumirpc::CallResponse>, Status> {
        self.captured.lock().unwrap().calls.push(request.into_inner());
        Ok(Response::new(pulumirpc::CallResponse::default()))
    }

    async fn supports_feature(
        &self,
        _: Request<pulumirpc::SupportsFeatureRequest>,
    ) -> Result<Response<pulumirpc::SupportsFeatureResponse>, Status> {
        Ok(Response::new(pulumirpc::SupportsFeatureResponse { has_support: true }))
    }

    async fn register_resource_outputs(
        &self,
        _: Request<pulumirpc::RegisterResourceOutputsRequest>,
    ) -> Result<Response<()>, Status> {
        Ok(Response::new(()))
    }

    async fn get_deployment_info(
        &self,
        _: Request<()>,
    ) -> Result<Response<pulumirpc::DeploymentInfo>, Status> {
        Err(Status::unimplemented("get_deployment_info"))
    }

    async fn register_stack_transform(
        &self,
        _: Request<pulumirpc::Callback>,
    ) -> Result<Response<()>, Status> {
        Ok(Response::new(()))
    }

    async fn register_stack_invoke_transform(
        &self,
        _: Request<pulumirpc::Callback>,
    ) -> Result<Response<()>, Status> {
        Ok(Response::new(()))
    }

    async fn register_resource_hook(
        &self,
        _: Request<pulumirpc::RegisterResourceHookRequest>,
    ) -> Result<Response<()>, Status> {
        Ok(Response::new(()))
    }

    async fn register_error_hook(
        &self,
        _: Request<pulumirpc::RegisterErrorHookRequest>,
    ) -> Result<Response<()>, Status> {
        Ok(Response::new(()))
    }

    async fn register_package(
        &self,
        _: Request<pulumirpc::RegisterPackageRequest>,
    ) -> Result<Response<pulumirpc::RegisterPackageResponse>, Status> {
        Ok(Response::new(pulumirpc::RegisterPackageResponse::default()))
    }

    async fn signal_and_wait_for_shutdown(
        &self,
        _: Request<()>,
    ) -> Result<Response<()>, Status> {
        Ok(Response::new(()))
    }
}

/// Start a fake monitor on an ephemeral port and return a context wired to
/// it, plus the capture buffer.
pub(crate) async fn fake_monitor_context()
-> (crate::Context, Arc<Mutex<Captured>>) {
    use crate::pulumirpc::resource_monitor_client::ResourceMonitorClient;
    use crate::pulumirpc::resource_monitor_server::ResourceMonitorServer;

    let captured = Arc::new(Mutex::new(Captured::default()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let service = FakeMonitor { captured: captured.clone() };
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    tokio::spawn(async move {
        let _ = tonic::transport::Server::builder()
            .add_service(ResourceMonitorServer::new(service))
            .serve_with_incoming(incoming)
            .await;
    });

    let channel = tonic::transport::Channel::from_shared(format!("http://127.0.0.1:{port}"))
        .unwrap()
        .connect()
        .await
        .expect("connecting to the fake monitor");
    let inner = Arc::new(crate::context::ContextInner {
        monitor: ResourceMonitorClient::new(channel),
        engine: None,
        settings: crate::context::RunSettings {
            project: "proj".to_string(),
            stack: "dev".to_string(),
            ..Default::default()
        },
        features: crate::context::Features {
            secrets: true,
            resource_references: true,
            output_values: true,
            invoke_depends_on: false,
        },
        config: crate::Config::default(),
        stack_urn: tokio::sync::OnceCell::new(),
        callbacks: tokio::sync::OnceCell::new(),
        package_refs: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        hydrated: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        pending: Mutex::new(vec![]),
        exports: Mutex::new(vec![]),
    });
    (crate::Context { inner }, captured)
}
