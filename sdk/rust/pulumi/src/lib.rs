//! Experimental Pulumi SDK for Rust.
//!
//! This crate provides the runtime a Pulumi Rust program uses to talk to the
//! Pulumi engine: connecting to the resource monitor, registering resources,
//! flowing `Output` values between them, and exporting stack outputs.

pub mod callbacks;
pub mod config;
pub mod context;
pub mod convert;
pub mod error;
pub mod hooks;
#[cfg(test)]
mod monitor_test_support;
pub mod ops;
pub mod output;
pub mod policy;
pub mod provider;
pub mod pv;
pub mod runtime;
pub mod stack_reference;
pub mod value;

pub use config::Config;
pub use context::{
    Alias, AliasParent, AliasSpec, Context, CustomTimeouts, InvokeOptions, PackageDescriptor,
    RegisterRequest, Resource, ResourceOptions,
};
pub use convert::{FromPropertyValue, IntoPropertyValue};
pub use error::{Error, Result};
pub use hooks::{ResourceHook, ResourceHookBinding};
pub use output::{deferred_output, DeferredResolver, Output, OutputData};
pub use policy::{
    policy_main, policy_main_with, AnalyzerResource, ConfigSchema, EnforcementLevel, Policy,
    PolicyPack, ResourceRemediationArgs, ResourceValidationArgs, StackInfo, ViolationManager,
};
pub use provider::{
    component_provider_host, ComponentProviderOptions, ConstructArgs, ConstructResult,
};
pub use pv::{range_entries, RangeEntry};
pub use runtime::run;
pub use stack_reference::StackReference;
pub use value::{Archive, Asset, AssetOrArchive, PropertyMap, PropertyValue};

/// Generated gRPC bindings for the Pulumi engine protocol.
pub mod pulumirpc {
    #![allow(clippy::all)]
    tonic::include_proto!("pulumirpc");
}
