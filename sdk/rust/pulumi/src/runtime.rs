//! Program entrypoint: wires a Pulumi program up to the engine and runs it.

use std::collections::HashMap;
use std::future::Future;

use futures::future::FutureExt;
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
        "" | "null" => HashMap::new(),
        raw => serde_json::from_str(raw)
            .map_err(|e| Error::new(format!("parsing PULUMI_CONFIG: {e}")))?,
    };
    let config_secret_keys: Vec<String> = match env("PULUMI_CONFIG_SECRET_KEYS").as_str() {
        "" | "null" => vec![],
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

async fn supports_feature(monitor: &mut ResourceMonitorClient<Channel>, id: &str) -> Result<bool> {
    let resp = monitor
        .supports_feature(pulumirpc::SupportsFeatureRequest { id: id.to_string() })
        .await?;
    Ok(resp.into_inner().has_support)
}

/// Connect to the engine and build a [`Context`] ready to run a program.
/// Matches the engine's 400MiB gRPC message cap.
const MAX_RPC_MESSAGE_SIZE: usize = 1024 * 1024 * 400;

pub async fn connect_context(settings: RunSettings) -> Result<Context> {
    let monitor_channel = connect(&settings.monitor_addr).await?;
    let mut monitor = ResourceMonitorClient::new(monitor_channel)
        .max_decoding_message_size(MAX_RPC_MESSAGE_SIZE)
        .max_encoding_message_size(MAX_RPC_MESSAGE_SIZE);

    let engine = if settings.engine_addr.is_empty() {
        None
    } else {
        Some(
            EngineClient::new(connect(&settings.engine_addr).await?)
                .max_decoding_message_size(MAX_RPC_MESSAGE_SIZE)
                .max_encoding_message_size(MAX_RPC_MESSAGE_SIZE),
        )
    };

    let mut features = Features {
        secrets: supports_feature(&mut monitor, "secrets").await?,
        resource_references: supports_feature(&mut monitor, "resourceReferences").await?,
        output_values: supports_feature(&mut monitor, "outputValues").await?,
        invoke_depends_on: false,
    };
    // Newer monitors advertise protocol features through GetDeploymentInfo;
    // older ones return Unimplemented, which leaves the flags off.
    if let Ok(info) = monitor.get_deployment_info(()).await {
        let info = info.into_inner();
        features.invoke_depends_on = info
            .supported_features
            .iter()
            .any(|f| *f == pulumirpc::ResourceMonitorFeature::InvokeDependsOn as i32);
    }

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
        callbacks: tokio::sync::OnceCell::new(),
        package_refs: tokio::sync::Mutex::new(HashMap::new()),
        hydrated: tokio::sync::Mutex::new(HashMap::new()),
        pending: Mutex::new(vec![]),
        exports: Mutex::new(vec![]),
    };
    let inner = Arc::new(inner);
    crate::context::set_active(inner.clone());
    Ok(Context { inner })
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

/// The message a panic carried, for the panics the SDK raises itself.
///
/// `panic!("{e}")` and `panic!("literal")` land here as a `String` or a
/// `&'static str` respectively; anything else is a panic from a dependency
/// and only its location was printed by the default hook.
fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<String>() {
        return s.clone();
    }
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        return s.to_string();
    }
    "the program panicked".to_string()
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

    // A panic anywhere in the program has to reach the engine as a
    // diagnostic and exit 32, the same as a returned error. Without this it
    // escapes as exit 101 and a raw Rust backtrace on stderr, which the host
    // reports as "Program exited with non-zero exit code: 101" with no clue
    // what went wrong. The SDK panics deliberately in a few places where
    // there is no error channel to return through — a config value of the
    // wrong type, `singleOrNone` on a longer list, an output whose shape does
    // not match the generated struct — and each of those should read as an
    // ordinary program failure.
    let result: Result<()> = match program_body(&ctx, program).await {
        Ok(result) => result,
        Err(panic_msg) => Err(Error::new(panic_msg)),
    };

    match result {
        Ok(()) => {
            // Let the engine finish anything that depends on the program
            // staying alive; older engines don't implement this.
            let mut monitor = ctx.inner.monitor.clone();
            let _ = monitor.signal_and_wait_for_shutdown(()).await;
            0
        }
        Err(e) => {
            ctx.log_error(format!("an unhandled error occurred: {e}"))
                .await;
            EXIT_STATUS_LOGGED_ERROR
        }
    }
}

/// The lifecycle both entrypoints share: register the stack resource, run
/// the program, drain registrations and publish outputs. The outer `Err`
/// is a caught panic's message; the inner result is the program's own.
async fn program_body<F, Fut>(ctx: &Context, program: F) -> std::result::Result<Result<()>, String>
where
    F: FnOnce(Context) -> Fut,
    Fut: Future<Output = Result<()>>,
{
    std::panic::AssertUnwindSafe(async {
        register_stack(ctx).await?;
        let program_err = program(ctx.clone()).await.err();
        // Publish stack outputs even when the program body errored.
        let finish_err = ctx.finish().await.err();
        match program_err.or(finish_err) {
            Some(e) => Err(e),
            None => Ok(()),
        }
    })
    .catch_unwind()
    .await
    .map_err(|payload| panic_message(&payload))
}

/// Serializes inline program runs. The crate keeps one process-global
/// "active context" slot for resource-reference hydration; two inline
/// programs running at once would cross-wire through it, so operations
/// queue here instead. Local-program operations are unaffected.
static INLINE_RUN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Run an inline automation-API program against explicit settings, inside
/// this process. Unlike [`run`], connection failures and program failures
/// alike come back as errors — the in-process language host reports them
/// to the engine in-band rather than exiting anything.
pub(crate) async fn run_inline(
    settings: RunSettings,
    program: crate::auto::ProgramFn,
) -> Result<()> {
    let _serialized = INLINE_RUN.lock().await;
    let ctx = connect_context(settings).await?;
    match program_body(&ctx, |ctx| program(ctx)).await {
        Ok(Ok(())) => {
            let mut monitor = ctx.inner.monitor.clone();
            let _ = monitor.signal_and_wait_for_shutdown(()).await;
            Ok(())
        }
        Ok(Err(e)) => Err(e),
        // The marker mirrors the Go SDK's "go inline source runtime error",
        // which the error-classification predicates look for.
        Err(panic_msg) => Err(Error::new(format!(
            "rust inline source runtime error, an unhandled panic occurred: {panic_msg}"
        ))),
    }
}
