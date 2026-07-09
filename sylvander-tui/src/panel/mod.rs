//! Panel components — render-only regions of the layout.
//!
//! Each panel is a unit struct implementing `Component`. Add a new panel
//! by creating a new file here and registering it in `AppState::register_default_panels`.

pub mod chat;
pub mod help;
pub mod input;
pub mod status;

pub use chat::ChatPanel;
pub use help::HelpPanel;
pub use input::InputPanel;
pub use status::StatusPanel;