//! Native shell for the Sylvander desktop presentation client.

use tauri_plugin_window_state::{Builder as WindowStateBuilder, StateFlags};

mod gateway;
mod host;

fn main() {
    tauri::Builder::default()
        // Window geometry is host presentation state. The official plugin
        // restores it before React starts and persists it without exposing a
        // filesystem or plugin command surface to the WebView.
        .plugin(
            WindowStateBuilder::default()
                .with_state_flags(StateFlags::SIZE | StateFlags::POSITION | StateFlags::MAXIMIZED)
                .build(),
        )
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .setup(host::setup)
        .manage(gateway::DesktopGateway::default())
        .invoke_handler(tauri::generate_handler![
            gateway::connect_runtime,
            gateway::disconnect_runtime,
            gateway::submit_runtime,
            host::get_host_preferences,
            host::save_user_profile_export,
            host::set_turn_notifications,
        ])
        .run(tauri::generate_context!())
        .expect("Sylvander desktop shell failed");
}
