//! Native shell for the Sylvander desktop presentation client.

use tauri_plugin_window_state::{Builder as WindowStateBuilder, StateFlags};

mod gateway;

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
        .manage(gateway::DesktopGateway::default())
        .invoke_handler(tauri::generate_handler![
            gateway::connect_runtime,
            gateway::disconnect_runtime,
            gateway::submit_runtime,
        ])
        .run(tauri::generate_context!())
        .expect("Sylvander desktop shell failed");
}
