pub mod chat;
pub mod commands;
pub mod knowledge;
pub mod shell;
pub mod tui;

use clap::{Parser, Subcommand};

pub use commands::handle_command;

#[derive(Parser)]
#[command(name = "agenta")]
#[command(about = "AI Agent Management CLI")]
#[command(version)]
#[command(subcommand_required = false, arg_required_else_help = false)]
pub struct Cli {
    #[arg(short, long, help = "Configuration file path")]
    pub config: Option<String>,

    #[arg(short, long, help = "Output format (json, table, yaml)", default_value = "table")]
    pub output: String,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Launch interactive shell
    Shell,

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
        #[arg(short = 'x', long, default_value = "once")]
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

        /// Enable memory (agent recalls past executions)
        #[arg(long)]
        memory: bool,

        /// Model provider override (e.g., ollama, deepseek, openrouter, openai)
        #[arg(long)]
        provider: Option<String>,

        /// Tool definitions (comma-separated file paths)
        #[arg(long)]
        tools: Option<String>,

        /// Permit this agent to run destructive tools autonomously (default: off)
        #[arg(long)]
        allow_destructive_tools: bool,

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

        /// Load the new system prompt from a file (avoids shell-quoting a long prompt)
        #[arg(long, value_name = "FILE")]
        prompt_file: Option<String>,

        /// New description
        #[arg(short, long)]
        description: Option<String>,

        /// New temperature
        #[arg(long)]
        temperature: Option<f32>,

        /// New max tokens
        #[arg(long)]
        max_tokens: Option<u32>,

        /// New execution mode
        #[arg(long)]
        mode: Option<String>,

        /// New schedule
        #[arg(long)]
        schedule: Option<String>,

        /// Directive passed as input on each scheduled tick (e.g. "send a break reminder")
        #[arg(long)]
        scheduled_input: Option<String>,

        /// Enable or disable memory
        #[arg(long)]
        memory: Option<bool>,

        /// Model provider override (e.g., ollama, deepseek, openrouter, openai)
        #[arg(long)]
        provider: Option<String>,

        /// Replace all tools from file paths (comma-separated)
        #[arg(long)]
        tools: Option<String>,

        /// Enable or disable deep-agent mode (multi-step reasoning + builder builtins)
        #[arg(long)]
        deep: Option<bool>,

        /// Deep agent max iterations (only when enabling deep mode)
        #[arg(long, default_value = "10")]
        deep_iterations: u32,

        /// Add (or update) a single installed tool by name, e.g. --add-tool tavily_search
        #[arg(long, value_name = "TOOL_NAME")]
        add_tool: Option<String>,

        /// Remove a tool from the agent by name, e.g. --remove-tool tavily_search
        #[arg(long, value_name = "TOOL_NAME")]
        remove_tool: Option<String>,

        /// Attach a knowledge base for RAG, e.g. --add-kb islamic-texts
        #[arg(long, value_name = "KB_NAME")]
        add_kb: Option<String>,

        /// Detach a knowledge base by name, e.g. --remove-kb islamic-texts
        #[arg(long, value_name = "KB_NAME")]
        remove_kb: Option<String>,

        /// RAG retrieval top-k for this agent — how many knowledge passages to
        /// inject per query. Overrides the global `rag_top_k` (default 8).
        #[arg(long, value_name = "N")]
        top_k: Option<usize>,

        /// Permit (or forbid) this agent to run destructive tools autonomously.
        #[arg(long)]
        allow_destructive_tools: Option<bool>,

        /// Custom sub-agent spawn notification message (deep agents only).
        /// Use {task} as a placeholder for the task description.
        /// Example: "🪸 Deploying REEF sub-agent: {task}"
        #[arg(long)]
        spawn_message: Option<String>,
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

        /// Overwrite existing agents with the same name
        #[arg(short, long)]
        force: bool,
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

    /// Script management commands
    Script {
        #[command(subcommand)]
        command: ScriptCommands,
    },

    /// Open the TUI dashboard (bare `agenta` opens the MIND chat instead)
    Dashboard,

    /// Review pending proposals from agents (e.g. MIND). Bare = list pending.
    Proposals {
        /// Show proposals of every status, not just pending
        #[arg(short, long)]
        all: bool,
        #[command(subcommand)]
        command: Option<ProposalCommands>,
    },

    /// Manage MIND's corrective memory — feedback & preferences it honors on every run
    Memory {
        #[command(subcommand)]
        command: Option<MemoryCommands>,
    },

    /// Approve and apply a pending proposal
    Approve {
        /// Proposal id (or a unique prefix)
        id: String,
    },

    /// Reject a pending proposal without applying it
    Reject {
        /// Proposal id (or a unique prefix)
        id: String,
        /// Optional reason, recorded on the proposal
        #[arg(short, long)]
        reason: Option<String>,
    },

    /// View runtime data
    View {
        #[command(subcommand)]
        command: ViewCommands,
    },

    /// Upgrade agenta to the latest version (or a specific version)
    Upgrade {
        /// Target version (e.g. v1.0.6). Defaults to latest.
        #[arg(default_value = "latest")]
        version: String,
    },

    /// Uninstall agenta: stop the daemon and remove its binaries
    Uninstall {
        /// Also delete config + data: ~/.agenta (config, .env, tools) and the
        /// local database/socket dir. Does NOT touch an external Postgres database.
        #[arg(long)]
        purge: bool,
        /// Skip the confirmation prompt.
        #[arg(short = 'y', long)]
        yes: bool,
    },

    /// Run diagnostics and check system health
    Doctor,

    /// First-time setup wizard (run without args), or configure a sub-system
    Setup {
        #[command(subcommand)]
        target: Option<SetupCommands>,
    },

    /// Pull a tool or agent from the agenta registry
    Pull {
        #[command(subcommand)]
        target: PullCommands,
    },

    /// Manage knowledge bases for RAG (Postgres/pgvector)
    Knowledge {
        #[command(subcommand)]
        command: KnowledgeCommands,
    },
}

#[derive(Subcommand)]
pub enum KnowledgeCommands {
    /// Create a knowledge base
    Create {
        /// Knowledge base name
        name: String,
        /// Embedder spec (provider:model)
        #[arg(long, default_value = "ollama:bge-m3")]
        embedder: String,
    },
    /// Ingest a file (.pdf/.md/.txt) into a knowledge base
    Add {
        /// Knowledge base name
        name: String,
        /// Path to the file
        file: String,
        /// Skip the extraction preview confirmation
        #[arg(short, long)]
        yes: bool,
        /// OCR the PDF with a vision model instead of text extraction (for
        /// image-based text, e.g. Arabic/scanned). Spec: provider:model, e.g.
        /// --ocr openrouter:qwen/qwen3-vl-32b-instruct
        #[arg(long, value_name = "PROVIDER:MODEL")]
        ocr: Option<String>,
        /// Chunking strategy: "words" (fixed windows, default) or "entries"
        /// (one chunk per numbered supplication/hadith with its section header
        /// attached — best for structured reference texts like Hisnul Muslim).
        #[arg(long, default_value = "words")]
        chunk_strategy: String,
    },
    /// List knowledge bases
    List,
    /// Delete a knowledge base and all its chunks
    Remove {
        /// Knowledge base name
        name: String,
    },
}

#[derive(Subcommand)]
pub enum PullCommands {
    /// Install a tool from agenta-tools registry
    Tool {
        /// Tool name (e.g. tavily_search)
        name: String,

        /// Version tag or branch (default: main)
        #[arg(long, default_value = "main")]
        version: String,

        /// Attach the tool to an agent after installing
        #[arg(long, value_name = "AGENT")]
        attach: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum SetupCommands {
    /// Add or update a Telegram bot for an agent
    Telegram,
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
pub enum ProposalCommands {
    /// Show a proposal's full preview + rationale
    Show {
        /// Proposal id (or a unique prefix)
        id: String,
    },
}

#[derive(Subcommand)]
pub enum MemoryCommands {
    /// Add a memory (a correction/preference an agent should honor)
    Add {
        /// The memory content
        content: String,
        /// Which agent it applies to
        #[arg(long, default_value = "MIND")]
        scope: String,
        /// Category: preference | correction | note
        #[arg(long, default_value = "note")]
        kind: String,
    },
    /// List memories for an agent (default: MIND)
    List {
        #[arg(long, default_value = "MIND")]
        scope: String,
        /// Include inactive memories too
        #[arg(short, long)]
        all: bool,
    },
    /// Remove a memory by id (or a unique prefix)
    Rm {
        id: String,
    },
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
        /// Environment variable this tool is allowed to read (repeatable).
        /// Everything not listed is withheld from the handler.
        #[arg(long = "secret")]
        secrets: Vec<String>,
        /// Effect classification: read-only | write | destructive (default read-only)
        #[arg(long, default_value = "read-only")]
        side_effect: String,
        /// Make this an HTTP tool: --handler is the request URL (no script is spawned).
        #[arg(long)]
        http: bool,
        /// HTTP method for --http tools (default POST).
        #[arg(long, default_value = "POST")]
        http_method: String,
        /// HTTP header for --http tools, "Key: Value" (repeatable). Values may
        /// reference an allowlisted secret as ${NAME}, e.g. "Authorization: Bearer ${TAVILY_API_KEY}".
        #[arg(long = "http-header")]
        http_headers: Vec<String>,
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
        /// Replace the secret allowlist (repeatable). Omit to leave unchanged.
        #[arg(long = "secret")]
        secrets: Vec<String>,
        /// Effect classification: read-only | write | destructive. Omit to leave unchanged.
        #[arg(long)]
        side_effect: Option<String>,
        /// Set the HTTP method (implies HTTP tool). Omit to leave unchanged.
        #[arg(long)]
        http_method: Option<String>,
        /// Replace HTTP headers, "Key: Value" (repeatable, implies HTTP tool).
        #[arg(long = "http-header")]
        http_headers: Vec<String>,
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
        /// Skip the confirmation prompt for write/destructive tools
        #[arg(short = 'y', long)]
        yes: bool,
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

#[derive(Subcommand)]
pub enum ScriptCommands {
    /// Create a new scheduled script
    Create {
        /// Script name
        #[arg(short, long)]
        name: String,
        /// Path to handler script (e.g. ~/.agenta/scripts/fetch.sh)
        #[arg(long)]
        handler: String,
        /// Optional description
        #[arg(short, long)]
        description: Option<String>,
        /// Cron schedule expression (e.g. "0 8 * * 1")
        #[arg(long)]
        schedule: Option<String>,
    },
    /// Get script details by ID or name
    Get {
        id: String,
    },
    /// List all scripts
    List,
    /// Update a script
    Update {
        id: String,
        #[arg(short, long)]
        name: Option<String>,
        #[arg(long)]
        handler: Option<String>,
        #[arg(short, long)]
        description: Option<String>,
        #[arg(long)]
        schedule: Option<String>,
        #[arg(long)]
        enabled: Option<bool>,
    },
    /// Delete a script
    Delete {
        id: String,
        #[arg(short, long)]
        force: bool,
    },
    /// Run a script manually
    Run {
        id: String,
        /// Wait for completion and print output
        #[arg(short, long)]
        wait: bool,
    },
    /// View script execution logs
    Logs {
        script_id: String,
        #[arg(short, long)]
        execution_id: Option<String>,
        #[arg(short, long, default_value = "20")]
        lines: usize,
    },
}
