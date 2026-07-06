// Modules are shared with the lib; the CLI binary only uses cli + core.
// Suppress dead_code / unused_imports for items used by the daemon binary.
#![allow(dead_code, unused_imports)]

mod cli;
mod core;
mod knowledge;
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
            // Quiet sqlx's "relation already exists" NOTICE spam on CLI commands
            // (knowledge ops re-run schema init each time). Overridable via RUST_LOG.
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "info,sqlx=warn".to_string()),
        )
        .init();

    let cli = Cli::parse();

    // Load ~/.agenta/.env so $VAR provider keys (e.g. OpenRouter for OCR) resolve.
    core::load_agenta_env();

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
            // Bare `agenta` opens the MIND chat. First-run guard: if MIND doesn't
            // exist yet, point the user at setup. (The TUI dashboard is now
            // `agenta dashboard`.)
            if !cli::commands::mind_exists(&config).await {
                eprintln!("Looks like this is your first time here.");
                eprintln!("Run {} to get started.", "agenta setup");
                std::process::exit(0);
            }
            if let Err(e) = cli::chat::run_chat(&config).await {
                eprintln!("Chat error: {}", e);
                std::process::exit(1);
            }
        }
    }
}
