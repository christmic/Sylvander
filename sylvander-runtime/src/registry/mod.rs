//! Runtime-owned Agent, provider, model, and credential revision governance.

#[allow(dead_code)] // production handler wiring follows the audited transport seam
pub(crate) mod administration;
pub mod agent;
#[allow(dead_code)] // pure bootstrap plan; executor wiring follows registry snapshots
pub(crate) mod bootstrap;
pub mod cognition_activation;
#[allow(dead_code)] // versioned composition is wired into Agent construction next
pub(crate) mod composition;
#[allow(dead_code)] // consumed by the staged registry mutation batches
pub(crate) mod domain;
#[allow(dead_code)] // versioned contract staged before SQL composition wiring
pub(crate) mod snapshot;
