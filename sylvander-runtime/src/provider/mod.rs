//! Provider catalogs, registry state, and request-scoped credential routing.

pub mod catalog_sync;
#[allow(dead_code)] // internal API consumed by model routing/admin batches
pub(crate) mod model_registry;
#[allow(dead_code)] // internal API consumed by provider routing/admin batches
pub(crate) mod registry;
#[allow(dead_code)] // wired by registry-backed composition after snapshot resolution
pub(crate) mod request_scoped;
