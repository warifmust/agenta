// Modules are shared with the lib; the CLI binary only uses cli + core.
// Suppress dead_code / unused_imports for items used by the daemon binary.
#![allow(dead_code, unused_imports)]

mod cli;
mod core;
mod ollama;
mod providers;
mod scheduler;
mod trigger;
mod tools;

use clap::Parser;
use cli::{handle_command, Cli};
use core::AppConfig;

#[tokio::main]
async fn main() {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "info".to_string()),
        )
        .init();

    let cli = Cli::parse();

    // Load configuration
    let config = match AppConfig::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to load configuration: {}", e);
            std::process::exit(1);
        }
    };

    match cli.command {
        Some(cmd) => {
            if let Err(e) = handle_command(cmd, config).await {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        None => {
            // No subcommand — open TUI
            if let Err(e) = cli::tui::run_tui(config).await {
                eprintln!("TUI error: {}", e);
                std::process::exit(1);
            }
        }
    }
}
