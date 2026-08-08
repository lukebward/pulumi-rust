//! Program entrypoint: wires a Pulumi program up to the engine and runs it.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use tonic::transport::{Channel, Endpoint};

use crate::config::Config;
use crate::context::{Context, ContextInner, Features, RunSettings};
use crate::error::{Error, Result};
use crate::pulumirpc;
use crate::pulumirpc::engine_client::EngineClient;
use crate::pulumirpc::resource_monitor_client::ResourceMonitorClient;

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}

/// Read the run settings the language host passes via the environment. The
/// variable names mirror the other Pulumi SDKs.
pub fn settings_from_env() -> Result<RunSettings> {
    let monitor_addr = env("PULUMI_MONITOR");
    if monitor_addr.is_empty() {
        return Err(Error::new(
            "PULUMI_MONITOR is not set; Pulumi programs must be run by the Pulumi CLI",
        ));
    }
    let config: HashMap<String, String> = match env("PULUMI_CONFIG").as_str() {
        "" => HashMap::new(),
        raw => serde_json::from_str(raw)
            .map_err(|e| Error::new(format!("parsing PULUMI_CONFIG: {e}")))?,
    };
    let config_secret_keys: Vec<String> = match env("PULUMI_CONFIG_SECRET_KEYS").as_str() {
        "" => vec![],
        raw => serde_json::from_str(raw)
            .map_err(|e| Error::new(format!("parsing PULUMI_CONFIG_SECRET_KEYS: {e}")))?,
    };
    let organization = match env("PULUMI_ORGANIZATION").as_str() {
        // Mirror the Go SDK's default when the engine passes no organization.
        "" => "organization".to_string(),
        o => o.to_string(),
    };
    Ok(RunSettings {
        project: env("PULUMI_PROJECT"),
        stack: env("PULUMI_STACK"),
        organization,
        dry_run: env("PULUMI_DRY_RUN") == "true",
        monitor_addr,
        engine_addr: env("PULUMI_ENGINE"),
        config,
        config_secret_keys,
    })
}

async fn connect(addr: &str) -> Result<Channel> {
    let uri = if addr.starts_with("http://") || addr.starts_with("https://") {
        addr.to_string()
    } else {
        format!("http://{addr}")
    };
    let endpoint = Endpoint::from_shared(uri)
        .map_err(|e| Error::new(format!("invalid address {addr:?}: {e}")))?;
    Ok(endpoint.connect().await?)
}

async fn supports_feature(
    monitor: &mut ResourceMonitorClient<Channel>,
    id: &str,
) -> Result<bool> {
    let resp = monitor
        .supports_feature(pulumirpc::SupportsFeatureRequest { id: id.to_string() })
        .await?;
    Ok(resp.into_inner().has_support)
}

/// Connect to the engine and build a [`Context`] ready to run a program.
pub async fn connect_context(settings: RunSettings) -> Result<Context> {
    let monitor_channel = connect(&settings.monitor_addr).await?;
    let mut monitor = ResourceMonitorClient::new(monitor_channel);

    let engine = if settings.engine_addr.is_empty() {
        None
    } else {
        Some(EngineClient::new(connect(&settings.engine_addr).await?))
    };

    let features = Features {
        secrets: supports_feature(&mut monitor, "secrets").await?,
        resource_references: supports_feature(&mut monitor, "resourceReferences").await?,
        output_values: supports_feature(&mut monitor, "outputValues").await?,
    };

    let config = Config::new(
        settings.config.clone(),
        settings.config_secret_keys.iter().cloned().collect(),
        settings.project.clone(),
    );

    let inner = ContextInner {
        monitor,
        engine,
        settings,
        features,
        config,
        stack_urn: tokio::sync::OnceCell::new(),
        pending: Mutex::new(vec![]),
        exports: Mutex::new(vec![]),
    };
    Ok(Context { inner: Arc::new(inner) })
}

/// Register the root stack resource.
async fn register_stack(ctx: &Context) -> Result<()> {
    let settings = &ctx.inner.settings;
    let request = pulumirpc::RegisterResourceRequest {
        r#type: "pulumi:pulumi:Stack".to_string(),
        name: format!("{}-{}", settings.project, settings.stack),
        custom: false,
        object: Some(crate::context::empty_struct()),
        accept_secrets: true,
        accept_resources: true,
        supports_partial_values: true,
        alias_specs: true,
        supports_result_reporting: true,
        ..Default::default()
    };
    let mut monitor = ctx.inner.monitor.clone();
    let response = monitor.register_resource(request).await?.into_inner();
    ctx.inner
        .stack_urn
        .set(response.urn)
        .map_err(|_| Error::new("stack registered twice"))?;
    Ok(())
}

/// The exit code signaling "the error was already logged to the engine";
/// the language host bails silently. Mirrors the other Pulumi SDKs.
pub const EXIT_STATUS_LOGGED_ERROR: i32 = 32;

/// Run a Pulumi program: the async closure receives a [`Context`], registers
/// resources, and exports stack outputs. Exits the process when done.
pub fn run<F, Fut>(program: F)
where
    F: FnOnce(Context) -> Fut,
    Fut: Future<Output = Result<()>>,
{
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");
    let code = rt.block_on(run_async(program));
    std::process::exit(code);
}

async fn run_async<F, Fut>(program: F) -> i32
where
    F: FnOnce(Context) -> Fut,
    Fut: Future<Output = Result<()>>,
{
    let settings = match settings_from_env() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let ctx = match connect_context(settings).await {
        Ok(ctx) => ctx,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };

    let result = async {
        register_stack(&ctx).await?;
        let program_err = program(ctx.clone()).await.err();
        // Publish stack outputs even when the program body errored.
        let finish_err = ctx.finish().await.err();
        match program_err.or(finish_err) {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
    .await;

    match result {
        Ok(()) => {
            // Let the engine finish anything that depends on the program
            // staying alive; older engines don't implement this.
            let mut monitor = ctx.inner.monitor.clone();
            let _ = monitor.signal_and_wait_for_shutdown(()).await;
            0
        }
        Err(e) => {
            ctx.log_error(format!("an unhandled error occurred: {e}")).await;
            EXIT_STATUS_LOGGED_ERROR
        }
    }
}
