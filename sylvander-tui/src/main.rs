//! Sylvander TUI binary entry point.
//!
//! Configuration and theme selection happen here; the event loop lives in
//! `runtime`, transport in `service`, and rendering in `ui`.

use sylvander_tui::config::TuiConfig;

#[tokio::main]
async fn main() {
    let config = TuiConfig::from_env_and_args().unwrap_or_else(|error| {
        eprintln!("sylvander-tui configuration error: {error}");
        std::process::exit(2);
    });
    sylvander_tui::theme::configure_color_capability(config.color_capability);
    sylvander_tui::theme::configure_overrides(config.theme_overrides);
    sylvander_tui::theme::configure(config.theme);
    sylvander_tui::theme::configure_accessibility(config.reduced_motion, config.no_italic);
    if let Err(error) = sylvander_tui::runtime::run(config).await {
        ratatui::restore();
        eprintln!("sylvander-tui runtime error: {error}");
        std::process::exit(1);
    }
}
