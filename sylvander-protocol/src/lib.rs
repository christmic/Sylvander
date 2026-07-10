//! # sylvander-protocol
//!
//! Wire-format protocol types for Sylvander's message bus.
//!
//! These types are language-neutral — they define the contract between
//! agents, channels, and the bus. All data types have `serde` and
//! `schemars::JsonSchema` derives.
//!
//! ## Multi-language
//!
//! The `types` module contains all cross-language data definitions.
//! `bus_trait` and `in_process` are Rust-only runtime types.
//!
//! ```bash
//! # Generate JSON Schema for TypeScript/Python/etc codegen
//! cargo run -p sylvander-protocol --features codegen -- --output schema/
//! ```

pub mod bus_trait;
pub mod in_process;
pub mod session_context;
pub mod types;

pub use types::*;
pub use bus_trait::{BusError, MessageBus, SubscriptionFilter};
pub use in_process::InProcessMessageBus;
pub use session_context::*;
