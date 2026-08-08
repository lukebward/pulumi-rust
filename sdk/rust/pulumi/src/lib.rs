//! Experimental Pulumi SDK for Rust.
//!
//! This crate provides the runtime a Pulumi Rust program uses to talk to the
//! Pulumi engine: connecting to the resource monitor, registering resources,
//! flowing `Output` values between them, and exporting stack outputs.

pub mod config;
pub mod context;
pub mod convert;
pub mod error;
pub mod ops;
pub mod output;
pub mod pv;
pub mod runtime;
pub mod stack_reference;
pub mod value;

pub use config::Config;
pub use context::{
    Alias, AliasParent, AliasSpec, Context, CustomTimeouts, InvokeOptions, RegisterRequest,
    Resource, ResourceOptions,
};
pub use convert::{FromPropertyValue, IntoPropertyValue};
pub use error::{Error, Result};
pub use output::{Output, OutputData};
pub use runtime::run;
pub use stack_reference::StackReference;
pub use value::{Archive, Asset, AssetOrArchive, PropertyMap, PropertyValue};

/// Generated gRPC bindings for the Pulumi engine protocol.
pub mod pulumirpc {
    #![allow(clippy::all)]
    tonic::include_proto!("pulumirpc");
}
