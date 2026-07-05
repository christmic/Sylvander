//! Built-in tool implementations.
//!
//! M3+ scope. M2 ships only the `Tool` trait and `MockTool` for tests.
//! M3 will add concrete file-system tools starting with `Read`.

pub mod read;

pub use read::ReadTool;