mod cli;
mod core;
mod ollama;
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

    if let Err(e) = handle_command(cli.command, config).await {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}
