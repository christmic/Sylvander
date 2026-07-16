//! Location-specific workspace executor adapters.

pub mod container;
pub mod ssh;

pub use container::ContainerExecutor;
pub use ssh::SshExecutor;
