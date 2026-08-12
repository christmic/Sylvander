//! Location-specific workspace executor adapters.

pub mod container;
mod local;
pub mod ssh;

pub use container::{ContainerExecutor, ContainerResourcePolicy};
pub(crate) use local::LocalExecutor;
pub use ssh::SshExecutor;
