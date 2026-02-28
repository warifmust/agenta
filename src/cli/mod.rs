pub mod commands;

use clap::{Parser, Subcommand};

pub use commands::handle_command;

#[derive(Parser)]
#[command(name = "agenta")]
#[command(about = "AI Agent Management CLI")]
#[command(version)]
pub struct Cli {
    #[arg(short, long, help = "Configuration file path")]
    pub config: Option<String>,

    #[arg(short, long, help = "Output format (json, table, yaml)", default_value = "table")]
    pub output: String,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Create a new agent
    Create {
        /// Agent name
        #[arg(short, long)]
        name: String,

        /// Model to use (e.g., llama2, mistral)
        #[arg(short, long, default_value = "llama2")]
        model: String,

        /// System prompt
        #[arg(short, long)]
        prompt: Option<String>,

        /// Load system prompt from file
        #[arg(long, value_name = "FILE")]
        prompt_file: Option<String>,

        /// Description
        #[arg(short, long)]
        description: Option<String>,

        /// Temperature (0.0 - 1.0)
        #[arg(long, default_value = "0.7")]
        temperature: f32,

        /// Top P (0.0 - 1.0)
        #[arg(long, default_value = "0.9")]
        top_p: f32,

        /// Max tokens
        #[arg(long, default_value = "2048")]
        max_tokens: u32,

        /// Execution mode (once, scheduled, triggered, continuous)
        #[arg(short, long, default_value = "once")]
        mode: String,

        /// Cron schedule (for scheduled mode)
        #[arg(long)]
        schedule: Option<String>,

        /// Enable deep agent mode
        #[arg(long)]
        deep: bool,

        /// Deep agent max iterations
        #[arg(long, default_value = "10")]
        deep_iterations: u32,

        /// Tool definitions (comma-separated file paths)
        #[arg(long)]
        tools: Option<String>,

        /// Interactive mode
        #[arg(short, long)]
        interactive: bool,
    },

    /// Get agent details
    Get {
        /// Agent ID or name
        id: String,

        /// Show full output
        #[arg(short, long)]
        full: bool,
    },

    /// List all agents
    List {
        /// Filter by status
        #[arg(short, long)]
        status: Option<String>,

        /// Show all fields
        #[arg(short, long)]
        all: bool,
    },

    /// Update an agent
    Update {
        /// Agent ID or name
        id: String,

        /// New name
        #[arg(short, long)]
        name: Option<String>,

        /// New model
        #[arg(short, long)]
        model: Option<String>,

        /// New system prompt
        #[arg(short, long)]
        prompt: Option<String>,

        /// New description
        #[arg(short, long)]
        description: Option<String>,

        /// New temperature
        #[arg(long)]
        temperature: Option<f32>,

        /// New execution mode
        #[arg(short, long)]
        mode: Option<String>,

        /// New schedule
        #[arg(long)]
        schedule: Option<String>,

        /// Tool definitions (comma-separated file paths)
        #[arg(long)]
        tools: Option<String>,
    },

    /// Delete an agent
    Delete {
        /// Agent ID or name
        id: String,

        /// Force deletion without confirmation
        #[arg(short, long)]
        force: bool,
    },

    /// Run an agent
    Run {
        /// Agent ID or name
        id: String,

        /// Input to the agent
        #[arg(short, long)]
        input: Option<String>,

        /// Read input from file
        #[arg(long, value_name = "FILE")]
        input_file: Option<String>,

        /// Wait for output
        #[arg(short, long)]
        wait: bool,

        /// Follow output
        #[arg(short, long)]
        follow: bool,
    },

    /// Stop a running agent
    Stop {
        /// Agent ID or name
        id: String,
    },

    /// View execution logs
    Logs {
        /// Agent ID or name
        agent_id: String,

        /// Execution ID
        #[arg(short, long)]
        execution_id: Option<String>,

        /// Number of lines to show
        #[arg(short, long, default_value = "50")]
        lines: usize,

        /// Follow new logs
        #[arg(short, long)]
        follow: bool,
    },

    /// Daemon management commands
    Daemon {
        #[command(subcommand)]
        command: DaemonCommands,
    },

    /// Import/export agents
    Export {
        /// Agent ID or 'all'
        id: String,

        /// Output file
        #[arg(short, long)]
        output: String,

        /// Format (json, yaml)
        #[arg(short, long, default_value = "json")]
        format: String,
    },

    /// Import agents from file
    Import {
        /// Input file
        #[arg(short, long)]
        input: String,

        /// Format (json, yaml)
        #[arg(short, long, default_value = "json")]
        format: String,
    },

    /// Shell completion
    Completion {
        /// Shell (bash, zsh, fish, powershell, elvish)
        shell: String,
    },

    /// Tool management commands
    Tool {
        #[command(subcommand)]
        command: ToolCommands,
    },

    /// View runtime data
    View {
        #[command(subcommand)]
        command: ViewCommands,
    },
}

#[derive(Subcommand)]
pub enum DaemonCommands {
    /// Start the daemon
    Start {
        /// Run in foreground
        #[arg(short, long)]
        foreground: bool,

        /// Log level
        #[arg(short, long, default_value = "info")]
        log_level: String,
    },

    /// Stop the daemon
    Stop {
        /// Force stop
        #[arg(short, long)]
        force: bool,
    },

    /// Check daemon status
    Status,

    /// Restart the daemon
    Restart,
}

#[derive(Subcommand)]
pub enum ToolCommands {
    /// Create a new tool
    Create {
        #[arg(short, long)]
        name: String,
        #[arg(short, long)]
        description: String,
        /// JSON schema for input parameters
        #[arg(long, default_value = "{\"type\":\"object\"}")]
        parameters: String,
        /// Command handler (e.g. \"/usr/bin/python3 /path/tool.py\")
        /// If omitted, a starter bash script is auto-created at ./tools/<name>.sh
        #[arg(long)]
        handler: Option<String>,
        /// Auto-generate a starter handler script (bash)
        #[arg(long)]
        scaffold: bool,
    },
    /// Get tool details by ID or name
    Get { id: String },
    /// List tools
    List,
    /// Update tool
    Update {
        id: String,
        #[arg(short, long)]
        name: Option<String>,
        #[arg(short, long)]
        description: Option<String>,
        #[arg(long)]
        parameters: Option<String>,
        #[arg(long)]
        handler: Option<String>,
        #[arg(long)]
        enabled: Option<bool>,
    },
    /// Delete tool
    Delete { id: String },
    /// Run tool manually
    Run {
        id: String,
        /// JSON input payload
        #[arg(short, long, default_value = "{}")]
        input: String,
        /// Wait for completion
        #[arg(short, long)]
        wait: bool,
    },
    /// View tool execution logs
    Logs {
        tool_id: String,
        #[arg(short, long)]
        execution_id: Option<String>,
        #[arg(short, long, default_value = "50")]
        lines: usize,
        #[arg(short, long)]
        follow: bool,
    },
}

#[derive(Subcommand)]
pub enum ViewCommands {
    /// List executions from database
    Executions {
        /// Maximum rows to show
        #[arg(short, long, default_value = "100")]
        limit: usize,
    },
}
