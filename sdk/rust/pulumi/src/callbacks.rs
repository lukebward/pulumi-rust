//! An in-process gRPC server the engine calls back into.
//!
//! Resource hooks run inside the program, so the SDK serves the
//! `pulumirpc.Callbacks` service and hands the engine its address. Each
//! registered callback gets a token the engine echoes back on invocation.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use tonic::{Request, Response, Status};

use crate::error::{Error, Result};
use crate::pulumirpc;
use crate::pulumirpc::callbacks_server::{Callbacks, CallbacksServer};

/// A callback body: takes the serialized request and returns the serialized
/// response.
pub(crate) type CallbackFn =
    Arc<dyn Fn(Vec<u8>) -> BoxFuture<'static, std::result::Result<Vec<u8>, Status>> + Send + Sync>;

#[derive(Default)]
struct Registry {
    callbacks: HashMap<String, CallbackFn>,
    next: u64,
}

/// A running callbacks server.
pub(crate) struct CallbackServer {
    target: String,
    registry: Arc<Mutex<Registry>>,
}

#[derive(Clone)]
struct Service {
    registry: Arc<Mutex<Registry>>,
}

#[tonic::async_trait]
impl Callbacks for Service {
    async fn invoke(
        &self,
        request: Request<pulumirpc::CallbackInvokeRequest>,
    ) -> std::result::Result<Response<pulumirpc::CallbackInvokeResponse>, Status> {
        let request = request.into_inner();
        let cb = {
            let registry = self.registry.lock().unwrap();
            registry.callbacks.get(&request.token).cloned()
        };
        let cb = cb.ok_or_else(|| {
            Status::not_found(format!("unknown callback token {}", request.token))
        })?;
        let response = cb(request.request).await?;
        Ok(Response::new(pulumirpc::CallbackInvokeResponse {
            response,
        }))
    }
}

impl CallbackServer {
    /// Bind an ephemeral local port and start serving.
    pub(crate) async fn start() -> Result<CallbackServer> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| Error::new(format!("binding callback server: {e}")))?;
        let addr = listener
            .local_addr()
            .map_err(|e| Error::new(format!("reading callback server address: {e}")))?;
        let registry = Arc::new(Mutex::new(Registry::default()));
        let service = Service {
            registry: registry.clone(),
        };
        let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
        tokio::spawn(async move {
            let _ = tonic::transport::Server::builder()
                .add_service(CallbacksServer::new(service))
                .serve_with_incoming(incoming)
                .await;
        });
        // The engine dials this with grpc.NewClient, which wants a bare
        // host:port.
        Ok(CallbackServer {
            target: format!("127.0.0.1:{}", addr.port()),
            registry,
        })
    }

    /// Register a callback and return the descriptor the engine needs.
    pub(crate) fn register(&self, cb: CallbackFn) -> pulumirpc::Callback {
        let token = {
            let mut registry = self.registry.lock().unwrap();
            registry.next += 1;
            let token = format!("callback-{}", registry.next);
            registry.callbacks.insert(token.clone(), cb);
            token
        };
        pulumirpc::Callback {
            target: self.target.clone(),
            token,
            accepts_byte_string: true,
        }
    }
}
