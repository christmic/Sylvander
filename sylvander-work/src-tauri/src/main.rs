//! Native shell for the Sylvander desktop presentation client.

mod gateway;

fn main() {
    tauri::Builder::default()
        .manage(gateway::DesktopGateway::default())
        .invoke_handler(tauri::generate_handler![
            gateway::connect_runtime,
            gateway::disconnect_runtime,
            gateway::submit_runtime,
        ])
        .run(tauri::generate_context!())
        .expect("Sylvander desktop shell failed");
}
