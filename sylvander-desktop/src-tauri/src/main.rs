//! Native shell for the Sylvander desktop presentation client.

fn main() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("Sylvander desktop shell failed");
}
