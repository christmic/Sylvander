//! Tool contracts, authorization, registration, and built-in implementations.

pub mod builtins;
mod contract;
pub mod invocation;
mod registry;

pub use contract::*;
pub use registry::{
    DynamicToolSource, ToolHookConfig, ToolRegistry, ToolRegistryCompositionError,
    build_definitions,
};

#[cfg(test)]
pub(crate) use registry::ToolTestExt;
