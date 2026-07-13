//! Panel components — render-only regions of the layout.
//!
//! Each panel is a unit struct implementing `Component`. Presentation owns the
//! component graph in `ui::dispatch`; application state never stores renderers.

pub mod chat;
pub mod header;
pub mod input;
pub mod status;

pub use chat::ChatPanel;
pub use header::HeaderPanel;
pub use input::InputPanel;
pub use status::StatusPanel;
