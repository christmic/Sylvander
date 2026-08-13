//! Runtime-owned credential resolution, revision state, and content-safe audit.

pub mod audit;
#[allow(dead_code)] // retained for credential administration and resolver batches
pub(crate) mod registry;
