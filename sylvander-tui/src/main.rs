//! Sylvander TUI binary entry point.
//!
//! Configuration and theme selection happen here; the event loop lives in
//! `runtime`, transport in `service`, and rendering in `ui`.

use sylvander_tui::config::TuiConfig;

const HELP: &str = "\
Sylvander terminal client

Usage: sylvander-tui [OPTIONS]

Options:
  --socket <PATH>       Sylvander Unix socket [default: /tmp/sylvander.sock]
  --session <ID>        Attach to one existing session
  --workspace <PATH>    Task workspace shown to the Agent
  -h, --help            Print help
  -V, --version         Print version

Appearance, editing, and key bindings are configured with SYLVANDER_TUI_*.
See sylvander-tui/docs/CONFIGURATION.md for the complete reference.
";

#[tokio::main]
async fn main() {
    if let Some(output) = informational_output(std::env::args().skip(1)) {
        print!("{output}");
        return;
    }
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

fn informational_output(args: impl IntoIterator<Item = String>) -> Option<String> {
    let args = args.into_iter().collect::<Vec<_>>();
    match args.as_slice() {
        [flag] if flag == "-h" || flag == "--help" => Some(HELP.into()),
        [flag] if flag == "-V" || flag == "--version" => {
            Some(format!("sylvander-tui {}\n", env!("CARGO_PKG_VERSION")))
        }
        _ => None,
    }
}

#[cfg(test)]
#[path = "../tests/unit/tui_main.rs"]
mod tests;
