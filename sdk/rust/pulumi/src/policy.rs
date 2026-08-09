//! Authoring and hosting policy packs.
//!
//! A policy pack is a program the engine runs as an analyzer plugin: it
//! serves the `Analyzer` service, describes its policies, and is called back
//! for each resource so it can report violations or remediate them.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use tonic::{Request, Response, Status};

use crate::error::{Error, Result};
use crate::pulumirpc;
use crate::pulumirpc::analyzer_server::{Analyzer, AnalyzerServer};
use crate::value::{marshal_properties, unmarshal_properties, PropertyMap, PropertyValue};

/// How strictly a policy is enforced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnforcementLevel {
    Advisory,
    Mandatory,
    Disabled,
    Remediate,
}

impl EnforcementLevel {
    fn to_proto(self) -> i32 {
        match self {
            EnforcementLevel::Advisory => 0,
            EnforcementLevel::Mandatory => 1,
            EnforcementLevel::Disabled => 2,
            EnforcementLevel::Remediate => 3,
        }
    }

    fn from_proto(v: i32) -> EnforcementLevel {
        match v {
            1 => EnforcementLevel::Mandatory,
            2 => EnforcementLevel::Disabled,
            3 => EnforcementLevel::Remediate,
            _ => EnforcementLevel::Advisory,
        }
    }
}

/// The resource a policy is being asked about.
#[derive(Clone, Debug)]
pub struct AnalyzerResource {
    pub type_: String,
    pub name: String,
    pub urn: String,
    pub properties: PropertyMap,
}

/// The stack a policy pack is analyzing.
#[derive(Clone, Debug, Default)]
pub struct StackInfo {
    pub project: String,
    pub stack: String,
    pub organization: String,
    pub dry_run: bool,
    pub config: HashMap<String, String>,
    pub tags: HashMap<String, String>,
}

/// Collects the violations a policy reports.
#[derive(Clone, Default)]
pub struct ViolationManager {
    violations: Arc<Mutex<Vec<(String, String)>>>,
}

impl ViolationManager {
    /// Report that the resource under analysis violates this policy.
    pub fn report_violation(&self, message: impl Into<String>, urn: impl Into<String>) {
        self.violations.lock().unwrap().push((message.into(), urn.into()));
    }

    fn take(&self) -> Vec<(String, String)> {
        std::mem::take(&mut *self.violations.lock().unwrap())
    }
}

/// What a resource validation receives.
pub struct ResourceValidationArgs {
    pub resource: AnalyzerResource,
    pub manager: ViolationManager,
    /// This policy's configuration, as configured for the stack.
    pub config: PropertyMap,
    /// The stack being analyzed.
    pub stack: StackInfo,
}

/// What a remediation receives; returning `Some` replaces the resource's
/// properties.
pub struct ResourceRemediationArgs {
    pub resource: AnalyzerResource,
    pub config: PropertyMap,
    pub stack: StackInfo,
}

type ValidateFn = Arc<dyn Fn(ResourceValidationArgs) -> BoxFuture<'static, Result<()>> + Send + Sync>;
type RemediateFn =
    Arc<dyn Fn(ResourceRemediationArgs) -> BoxFuture<'static, Result<Option<PropertyMap>>> + Send + Sync>;

/// A JSON-schema description of a policy's configuration.
#[derive(Clone, Debug, Default)]
pub struct ConfigSchema {
    /// Property name to JSON schema fragment.
    pub properties: BTreeMap<String, PropertyValue>,
    pub required: Vec<String>,
}

/// One policy in a pack.
#[derive(Clone)]
pub struct Policy {
    pub name: String,
    pub description: String,
    pub enforcement_level: EnforcementLevel,
    pub config_schema: Option<ConfigSchema>,
    validate: Option<ValidateFn>,
    remediate: Option<RemediateFn>,
}

impl Policy {
    /// A policy that inspects each resource and reports violations.
    pub fn resource_validation(
        name: impl Into<String>,
        description: impl Into<String>,
        enforcement_level: EnforcementLevel,
        validate: impl Fn(ResourceValidationArgs) -> BoxFuture<'static, Result<()>> + Send + Sync + 'static,
    ) -> Policy {
        Policy {
            name: name.into(),
            description: description.into(),
            enforcement_level,
            config_schema: None,
            validate: Some(Arc::new(validate)),
            remediate: None,
        }
    }

    /// A policy that rewrites a resource's properties instead of reporting.
    pub fn resource_remediation(
        name: impl Into<String>,
        description: impl Into<String>,
        remediate: impl Fn(ResourceRemediationArgs) -> BoxFuture<'static, Result<Option<PropertyMap>>>
            + Send
            + Sync
            + 'static,
    ) -> Policy {
        Policy {
            name: name.into(),
            description: description.into(),
            enforcement_level: EnforcementLevel::Remediate,
            config_schema: None,
            validate: None,
            remediate: Some(Arc::new(remediate)),
        }
    }

    /// Attach a configuration schema to this policy.
    pub fn with_config_schema(mut self, schema: ConfigSchema) -> Policy {
        self.config_schema = Some(schema);
        self
    }
}

/// A named, versioned collection of policies.
#[derive(Clone)]
pub struct PolicyPack {
    pub name: String,
    pub version: String,
    pub enforcement_level: EnforcementLevel,
    pub policies: Vec<Policy>,
}

impl PolicyPack {
    /// Build a policy pack, rejecting the same names the other SDKs do.
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        enforcement_level: EnforcementLevel,
        policies: Vec<Policy>,
    ) -> Result<PolicyPack> {
        let name = name.into();
        if name.is_empty()
            || name.len() > 100
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        {
            return Err(Error::new(format!("invalid policy pack name {name:?}")));
        }
        for policy in &policies {
            if policy.name == "all" {
                return Err(Error::new(
                    "invalid policy name \"all\". \"all\" is a reserved name".to_string(),
                ));
            }
            if let Some(schema) = &policy.config_schema {
                if schema.properties.contains_key("enforcementLevel")
                    || schema.required.iter().any(|r| r == "enforcementLevel")
                {
                    return Err(Error::new(format!(
                        "policy {}: enforcementLevel cannot appear in a config schema",
                        policy.name
                    )));
                }
            }
        }
        Ok(PolicyPack { name, version: version.into(), enforcement_level, policies })
    }
}

#[derive(Default)]
struct State {
    /// A factory-built pack, constructed once when the engine configures
    /// the stack, as the Go SDK does.
    pack: Option<PolicyPack>,
    /// Per-policy configuration, as the engine configured it.
    config: HashMap<String, (EnforcementLevel, PropertyMap)>,
    stack: StackInfo,
}

struct Service {
    source: PackSource,
    state: Mutex<State>,
}

impl Service {
    /// The pack, built from the stack configuration when the pack is
    /// produced by a factory. A factory failure surfaces as an error rather
    /// than a nameless empty pack, so the user sees the real cause.
    fn pack(&self) -> std::result::Result<PolicyPack, Status> {
        match &self.source {
            PackSource::Fixed(p) => Ok(p.clone()),
            PackSource::Factory(f) => {
                {
                    let state = self.state.lock().unwrap();
                    if let Some(pack) = &state.pack {
                        return Ok(pack.clone());
                    }
                }
                // Not configured yet (the engine asks for plugin info before
                // ConfigureStack); build from whatever we know so far.
                let stack = self.state.lock().unwrap().stack.clone();
                f(stack).map_err(|e| Status::internal(format!("building policy pack: {e}")))
            }
        }
    }
}

impl Service {
    fn policy_config(&self, pack: &PolicyPack, name: &str) -> (EnforcementLevel, PropertyMap) {
        let state = self.state.lock().unwrap();
        match state.config.get(name) {
            Some((level, props)) => (*level, props.clone()),
            None => (
                pack.policies
                    .iter()
                    .find(|p| p.name == name)
                    .map(|p| p.enforcement_level)
                    .unwrap_or(pack.enforcement_level),
                PropertyMap::new(),
            ),
        }
    }

    fn stack(&self) -> StackInfo {
        self.state.lock().unwrap().stack.clone()
    }

    async fn analyze_resource(
        &self,
        resource: AnalyzerResource,
    ) -> std::result::Result<Vec<pulumirpc::AnalyzeDiagnostic>, Status> {
        let mut diagnostics = vec![];
        let pack = self.pack()?;
        for policy in &pack.policies {
            let Some(validate) = &policy.validate else {
                continue;
            };
            let (level, config) = self.policy_config(&pack, &policy.name);
            if level == EnforcementLevel::Disabled {
                continue;
            }
            let manager = ViolationManager::default();
            validate(ResourceValidationArgs {
                resource: resource.clone(),
                manager: manager.clone(),
                config,
                stack: self.stack(),
            })
            .await
            .map_err(|e| Status::internal(format!("{e}")))?;
            for (message, urn) in manager.take() {
                // The engine shows `message` verbatim and never reads
                // `description`, so the description leads the message.
                let message = if message.is_empty() {
                    policy.description.clone()
                } else {
                    format!("{}\n{}", policy.description, message)
                };
                diagnostics.push(pulumirpc::AnalyzeDiagnostic {
                    policy_name: policy.name.clone(),
                    policy_pack_name: pack.name.clone(),
                    policy_pack_version: pack.version.clone(),
                    description: policy.description.clone(),
                    message,
                    enforcement_level: level.to_proto(),
                    urn: if urn.is_empty() { resource.urn.clone() } else { urn },
                    ..Default::default()
                });
            }
        }
        Ok(diagnostics)
    }
}

fn resource_from_proto(
    type_: String,
    name: String,
    urn: String,
    properties: Option<&prost_types::Struct>,
) -> AnalyzerResource {
    AnalyzerResource {
        type_,
        name,
        urn,
        properties: properties.map(unmarshal_properties).unwrap_or_default(),
    }
}

#[tonic::async_trait]
impl Analyzer for Service {
    async fn analyze(
        &self,
        request: Request<pulumirpc::AnalyzeRequest>,
    ) -> std::result::Result<Response<pulumirpc::AnalyzeResponse>, Status> {
        let r = request.into_inner();
        let resource =
            resource_from_proto(r.r#type, r.name, r.urn, r.properties.as_ref());
        let diagnostics = self.analyze_resource(resource).await?;
        Ok(Response::new(pulumirpc::AnalyzeResponse {
            diagnostics,
            not_applicable: vec![],
        }))
    }

    async fn analyze_stack(
        &self,
        _request: Request<pulumirpc::AnalyzeStackRequest>,
    ) -> std::result::Result<Response<pulumirpc::AnalyzeResponse>, Status> {
        // Resource policies have already run per resource; reporting them
        // again here would duplicate every violation. Stack-level policies
        // are a separate policy kind this SDK does not model yet.
        Ok(Response::new(pulumirpc::AnalyzeResponse::default()))
    }

    async fn remediate(
        &self,
        request: Request<pulumirpc::AnalyzeRequest>,
    ) -> std::result::Result<Response<pulumirpc::RemediateResponse>, Status> {
        let r = request.into_inner();
        // The engine applies each remediation as a full replacement of the
        // resource's inputs, in order, so every policy must see what the
        // previous one produced. Passing the original map to all of them
        // would silently revert all but the last.
        let mut resource =
            resource_from_proto(r.r#type, r.name, r.urn, r.properties.as_ref());
        let mut remediations = vec![];
        let pack = self.pack()?;
        for policy in &pack.policies {
            let Some(remediate) = &policy.remediate else {
                continue;
            };
            let (level, config) = self.policy_config(&pack, &policy.name);
            if level == EnforcementLevel::Disabled {
                continue;
            }
            let result = remediate(ResourceRemediationArgs {
                resource: resource.clone(),
                config,
                stack: self.stack(),
            })
            .await
            .map_err(|e| Status::internal(format!("{e}")))?;
            if let Some(props) = result {
                resource.properties = props.clone();
                remediations.push(pulumirpc::Remediation {
                    policy_name: policy.name.clone(),
                    policy_pack_name: pack.name.clone(),
                    policy_pack_version: pack.version.clone(),
                    description: policy.description.clone(),
                    properties: Some(marshal_properties(&props)),
                    diagnostic: String::new(),
                });
            }
        }
        Ok(Response::new(pulumirpc::RemediateResponse {
            remediations,
            not_applicable: vec![],
        }))
    }

    async fn get_analyzer_info(
        &self,
        _request: Request<()>,
    ) -> std::result::Result<Response<pulumirpc::AnalyzerInfo>, Status> {
        let pack = self.pack()?;
        let policies = pack
            .policies
            .iter()
            .map(|p| pulumirpc::PolicyInfo {
                name: p.name.clone(),
                display_name: p.name.clone(),
                description: p.description.clone(),
                enforcement_level: p.enforcement_level.to_proto(),
                config_schema: p.config_schema.as_ref().map(|s| {
                    pulumirpc::PolicyConfigSchema {
                        properties: Some(marshal_properties(&s.properties.clone())),
                        required: s.required.clone(),
                    }
                }),
                ..Default::default()
            })
            .collect();
        Ok(Response::new(pulumirpc::AnalyzerInfo {
            name: pack.name.clone(),
            display_name: pack.name.clone(),
            version: pack.version.clone(),
            policies,
            supports_config: true,
            ..Default::default()
        }))
    }

    async fn get_plugin_info(
        &self,
        _request: Request<()>,
    ) -> std::result::Result<Response<pulumirpc::PluginInfo>, Status> {
        Ok(Response::new(pulumirpc::PluginInfo {
            version: self.pack()?.version.clone(),
        }))
    }

    async fn configure(
        &self,
        request: Request<pulumirpc::ConfigureAnalyzerRequest>,
    ) -> std::result::Result<Response<()>, Status> {
        let r = request.into_inner();
        let mut state = self.state.lock().unwrap();
        state.config.clear();
        for (name, config) in r.policy_config {
            let props = config.properties.as_ref().map(unmarshal_properties).unwrap_or_default();
            state
                .config
                .insert(name, (EnforcementLevel::from_proto(config.enforcement_level), props));
        }
        Ok(Response::new(()))
    }

    async fn handshake(
        &self,
        _request: Request<pulumirpc::AnalyzerHandshakeRequest>,
    ) -> std::result::Result<Response<pulumirpc::AnalyzerHandshakeResponse>, Status> {
        Ok(Response::new(pulumirpc::AnalyzerHandshakeResponse::default()))
    }

    async fn configure_stack(
        &self,
        request: Request<pulumirpc::AnalyzerStackConfigureRequest>,
    ) -> std::result::Result<Response<pulumirpc::AnalyzerStackConfigureResponse>, Status> {
        let r = request.into_inner();
        let stack = StackInfo {
            project: r.project,
            stack: r.stack,
            organization: r.organization,
            dry_run: r.dry_run,
            config: r.config.into_iter().collect(),
            tags: r.tags.into_iter().collect(),
        };
        let built = match &self.source {
            PackSource::Factory(f) => Some(
                f(stack.clone())
                    .map_err(|e| Status::internal(format!("building policy pack: {e}")))?,
            ),
            PackSource::Fixed(_) => None,
        };
        let mut state = self.state.lock().unwrap();
        state.stack = stack;
        state.pack = built;
        Ok(Response::new(pulumirpc::AnalyzerStackConfigureResponse::default()))
    }

    async fn cancel(&self, _request: Request<()>) -> std::result::Result<Response<()>, Status> {
        Ok(Response::new(()))
    }
}

/// Serve a policy pack the engine configures first, so the pack can derive
/// itself from the stack's configuration. The engine issues ConfigureStack
/// before asking what policies exist.
pub async fn policy_main_with(
    factory: impl Fn(StackInfo) -> Result<PolicyPack> + Send + Sync + 'static,
) -> Result<()> {
    serve(PackSource::Factory(Arc::new(factory))).await
}

/// Serve a policy pack until the engine disconnects. The engine reads the
/// plugin's port from the first line of stdout, so nothing else is printed
/// there.
pub async fn policy_main(pack: PolicyPack) -> Result<()> {
    serve(PackSource::Fixed(pack)).await
}

enum PackSource {
    Fixed(PolicyPack),
    Factory(Arc<dyn Fn(StackInfo) -> Result<PolicyPack> + Send + Sync>),
}

async fn serve(source: PackSource) -> Result<()> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| Error::new(format!("binding analyzer server: {e}")))?;
    let port = listener
        .local_addr()
        .map_err(|e| Error::new(format!("reading analyzer address: {e}")))?
        .port();

    println!("{port}");
    use std::io::Write;
    let _ = std::io::stdout().flush();

    let service = Service { source, state: Mutex::new(State::default()) };
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);
    tonic::transport::Server::builder()
        .add_service(AnalyzerServer::new(service))
        .serve_with_incoming(incoming)
        .await
        .map_err(|e| Error::new(format!("serving analyzer: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(pack: PolicyPack) -> Service {
        Service { source: PackSource::Fixed(pack), state: Mutex::new(State::default()) }
    }

    fn props(pairs: &[(&str, &str)]) -> PropertyMap {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), PropertyValue::String(v.to_string())))
            .collect()
    }

    fn analyze_request(properties: &PropertyMap) -> Request<pulumirpc::AnalyzeRequest> {
        Request::new(pulumirpc::AnalyzeRequest {
            r#type: "test:index:Thing".to_string(),
            name: "thing".to_string(),
            urn: "urn:pulumi:dev::p::test:index:Thing::thing".to_string(),
            properties: Some(marshal_properties(properties)),
            ..Default::default()
        })
    }

    /// A policy that always reports one violation with the given message.
    fn failing(name: &str, description: &str, message: &'static str) -> Policy {
        failing_at(name, description, message, EnforcementLevel::Advisory)
    }

    fn failing_at(
        name: &str,
        description: &str,
        message: &'static str,
        level: EnforcementLevel,
    ) -> Policy {
        Policy::resource_validation(name, description, level, move |args: ResourceValidationArgs| {
            Box::pin(async move {
                args.manager.report_violation(message, "");
                Ok(())
            })
        })
    }

    /// A policy that rewrites one property.
    fn rewrite(name: &str, key: &'static str, value: &'static str) -> Policy {
        Policy::resource_remediation(name, "rewrites a property", move |args: ResourceRemediationArgs| {
            Box::pin(async move {
                let mut p = args.resource.properties.clone();
                p.insert(key.to_string(), PropertyValue::String(value.to_string()));
                Ok(Some(p))
            })
        })
    }

    #[tokio::test]
    async fn a_violation_message_leads_with_the_policy_description() {
        // The engine prints `message` verbatim and never reads `description`,
        // so a bare message loses the policy's own explanation.
        let pack = PolicyPack::new("p", "1.0.0", EnforcementLevel::Advisory,
                                   vec![failing("no-public", "buckets must be private", "it is public")])
            .unwrap();
        let diags = service(pack)
            .analyze_resource(AnalyzerResource {
                type_: "test:index:Thing".into(),
                name: "thing".into(),
                urn: "urn:thing".into(),
                properties: PropertyMap::new(),
            })
            .await
            .unwrap();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].message, "buckets must be private\nit is public");
        assert_eq!(diags[0].description, "buckets must be private");
        assert_eq!(diags[0].policy_name, "no-public");
    }

    #[tokio::test]
    async fn a_violation_with_no_message_is_just_the_description() {
        let pack = PolicyPack::new("p", "1.0.0", EnforcementLevel::Advisory,
                                   vec![failing("no-public", "buckets must be private", "")])
            .unwrap();
        let diags = service(pack)
            .analyze_resource(AnalyzerResource {
                type_: "t".into(), name: "n".into(), urn: "urn:n".into(),
                properties: PropertyMap::new(),
            })
            .await
            .unwrap();
        assert_eq!(diags[0].message, "buckets must be private");
    }

    #[tokio::test]
    async fn a_violation_defaults_to_the_analysed_resource_urn() {
        let pack = PolicyPack::new("p", "1.0.0", EnforcementLevel::Advisory,
                                   vec![failing("x", "d", "m")]).unwrap();
        let diags = service(pack)
            .analyze_resource(AnalyzerResource {
                type_: "t".into(), name: "n".into(), urn: "urn:the-resource".into(),
                properties: PropertyMap::new(),
            })
            .await
            .unwrap();
        assert_eq!(diags[0].urn, "urn:the-resource");
    }

    #[tokio::test]
    async fn remediations_chain_so_each_policy_sees_the_previous_result() {
        // The engine applies each remediation as a full replacement of the
        // inputs, in order. Handing every policy the *original* map means all
        // but the last are silently reverted — a corrupted deployment with no
        // error anywhere.
        let pack = PolicyPack::new(
            "p", "1.0.0", EnforcementLevel::Advisory,
            vec![rewrite("first", "a", "1"), rewrite("second", "b", "2")],
        )
        .unwrap();
        let resp = service(pack)
            .remediate(analyze_request(&props(&[("keep", "yes")])))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(resp.remediations.len(), 2);
        let last = unmarshal_properties(resp.remediations[1].properties.as_ref().unwrap());
        assert_eq!(last.get("keep"), Some(&PropertyValue::String("yes".into())));
        assert_eq!(last.get("a"), Some(&PropertyValue::String("1".into())),
                   "the second remediation dropped the first one's change");
        assert_eq!(last.get("b"), Some(&PropertyValue::String("2".into())));
    }

    #[tokio::test]
    async fn a_policy_that_remediates_nothing_produces_no_remediation() {
        let pack = PolicyPack::new(
            "p", "1.0.0", EnforcementLevel::Advisory,
            vec![Policy::resource_remediation("noop", "does nothing", |_: ResourceRemediationArgs| {
                Box::pin(async { Ok(None) })
            })],
        )
        .unwrap();
        let resp = service(pack)
            .remediate(analyze_request(&PropertyMap::new()))
            .await
            .unwrap()
            .into_inner();
        assert!(resp.remediations.is_empty());
    }

    #[tokio::test]
    async fn a_disabled_policy_neither_validates_nor_remediates() {
        let pack = PolicyPack::new(
            "p", "1.0.0", EnforcementLevel::Advisory,
            vec![failing("v", "d", "m"), rewrite("r", "a", "1")],
        )
        .unwrap();
        let svc = service(pack);
        {
            let mut state = svc.state.lock().unwrap();
            state.config.insert("v".into(), (EnforcementLevel::Disabled, PropertyMap::new()));
            state.config.insert("r".into(), (EnforcementLevel::Disabled, PropertyMap::new()));
        }
        let diags = svc
            .analyze_resource(AnalyzerResource {
                type_: "t".into(), name: "n".into(), urn: "urn:n".into(),
                properties: PropertyMap::new(),
            })
            .await
            .unwrap();
        assert!(diags.is_empty());
        let resp = svc.remediate(analyze_request(&PropertyMap::new())).await.unwrap().into_inner();
        assert!(resp.remediations.is_empty());
    }

    #[tokio::test]
    async fn configured_enforcement_overrides_the_policy_and_the_pack() {
        let pack = PolicyPack::new("p", "1.0.0", EnforcementLevel::Advisory,
                                   vec![failing("v", "d", "m")]).unwrap();
        let svc = service(pack);
        svc.state.lock().unwrap().config
            .insert("v".into(), (EnforcementLevel::Mandatory, PropertyMap::new()));
        let diags = svc
            .analyze_resource(AnalyzerResource {
                type_: "t".into(), name: "n".into(), urn: "urn:n".into(),
                properties: PropertyMap::new(),
            })
            .await
            .unwrap();
        assert_eq!(diags[0].enforcement_level, EnforcementLevel::Mandatory.to_proto());
    }

    #[tokio::test]
    async fn an_unconfigured_policy_falls_back_to_its_own_level_then_the_pack() {
        let own = failing_at("v", "d", "m", EnforcementLevel::Mandatory);
        let pack = PolicyPack::new("p", "1.0.0", EnforcementLevel::Advisory, vec![own]).unwrap();
        let (level, _) = service(pack.clone()).policy_config(&pack, "v");
        assert_eq!(level, EnforcementLevel::Mandatory);
        // A policy the pack does not know falls back to the pack's level.
        let (level, _) = service(pack.clone()).policy_config(&pack, "absent");
        assert_eq!(level, EnforcementLevel::Advisory);
    }

    #[test]
    fn a_config_schema_may_not_redeclare_enforcement_level() {
        // The engine owns that key; a pack declaring it would shadow the
        // engine's own handling.
        let mut schema = ConfigSchema::default();
        schema
            .properties
            .insert("enforcementLevel".to_string(), PropertyValue::String("string".into()));
        let policy = failing("v", "d", "m").with_config_schema(schema);
        let err = match PolicyPack::new("p", "1.0.0", EnforcementLevel::Advisory, vec![policy]) {
            Ok(_) => panic!("a pack redeclaring enforcementLevel was accepted"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("enforcementLevel"), "unhelpful error: {err}");
    }

    #[test]
    fn the_violation_manager_drains_so_a_policy_cannot_double_report() {
        let m = ViolationManager::default();
        m.report_violation("one", "urn:a");
        m.report_violation("two", "");
        assert_eq!(m.take().len(), 2);
        assert!(m.take().is_empty(), "violations were reported twice");
    }
}
