<p align="center">
  <img src="assets/agenta-logo-2.png" width="240" alt="Agenta Logo">
</p>

<h1 align="center">Agenta</h1>

<p align="center">
  <strong>Define it. Deploy it. Forget it.</strong>
</p>

<p align="center">
  Self-hosted runtime for managing, orchestrating, and observing AI agents. Define agents, attach tools, schedule runs, wire them together then watch them work. Ollama by default; swap to DeepSeek, OpenRouter, or OpenAI per agent, no re-architecture needed. Built-in file tools, deep reasoning, sub-agent delegation, Telegram, REST API. One binary. No cloud dependency. No vendor lock-in. Powered by <strong>Rust</strong>.
</p>

---

## ✨ What You Get

- 🖥️ **TUI dashboard** — run `agenta` to open the interactive dashboard: manage agents, chat with them, view details, all from the terminal
- 🤖 **Agent management** — `create`, `update`, `run`, `logs`, `list` from the CLI
- ⏰ **Scheduling** — cron-based scheduling baked into the daemon
- 🧠 **All agents are harnessed** — every agent runs in harness mode by default: multi-step reasoning, iterative tool use, built-in file tools, and memory — no flags, no config
- 🪄 **Sub-agent spawning** — agents can spin up ephemeral sub-agents or delegate to named agents at runtime
- 📁 **Built-in file tools** — `read_file`, `write_file`, `list_files` available to every agent, zero setup
- 💬 **Telegram integration** — multiple bots, one daemon, no webhook or tunnel needed
- 🧵 **Agent memory** — enabled by default, injects past outputs as context on every run
- 📦 **Export / import** — backup agents as JSON/YAML, auto-backup on every daemon start
- 🗄️ **SQLite by default**, Postgres optional
- 🌐 **REST API + Swagger UI** — built-in, no extra setup
- 🔌 **Pluggable model backends** — Ollama (local), DeepSeek, OpenRouter, OpenAI — per-agent override
- 🏠 **Fully self-hosted** — runs on your laptop, a cheap VPS, or a Raspberry Pi

---

## 🚀 Install & First-Time Setup

### 1. Prerequisites

- [Ollama](https://ollama.com) installed and running (if using local models)
- At least one model pulled

```bash
ollama pull qwen3:latest  # or any model
ollama ps
```

### 2. Install Agenta

**From GitHub (recommended):**

```bash
curl -fsSL https://raw.githubusercontent.com/warifmust/agenta/main/install.sh | bash
```

The installer builds and installs the binary, then launches the **setup wizard** automatically.

**From source:**

Requires Rust. Install via [rustup](https://rustup.rs) if `cargo` is not found:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

Then build and install:

```bash
cargo install --path . --force
agenta setup
```

**Custom install options:**

```bash
AGENTA_REPO="warifmust/agenta"
AGENTA_VERSION="latest"
AGENTA_INSTALL_DIR="$HOME/.local/bin"
./install.sh
```

### 3. Setup Wizard

`agenta setup` runs automatically after install. It guides you through three steps:

```
  Welcome to agenta!

  Step 1/3 — AI Provider
    1) Ollama   — local, no API key needed
    2) DeepSeek — cloud, fast & cheap
    3) OpenRouter — cloud, 300+ models
    4) OpenAI   — cloud, GPT models

  Step 2/3 — API Key
    API key: (saved to ~/.agenta/.env)

  Step 3/3 — Model
    Model: [press Enter for deepseek-chat]

  Set up Telegram bot? [y/N]:
```

At the end it:
- Writes `~/.agenta/config.toml`
- Starts the daemon
- Creates **MIND**, the system agent that manages tools and scripts

Run it again at any time to re-configure:

```bash
agenta setup
```

### 4. Add Telegram Later

```bash
agenta setup telegram
```

This walks you through connecting a Telegram bot to an existing agent — asks for the bot token, shows your agent list to pick from, and appends a `[[telegram_bots]]` entry to `config.toml`. Run it once per bot.

### 5. Open the Dashboard

```bash
agenta
```

---

## 🖥️ Dashboard

Run `agenta` with no arguments to open the interactive terminal dashboard — the primary way to manage and talk to your agents.

<p align="center">
  <img src="assets/agenta-dashboard.png" width="800" alt="Agenta Dashboard">
</p>

Two panels: **Agents** on the left, **Chat** on the right. Select an agent, type a message, watch it think and respond in real time.

### Keyboard Reference

| Key | Action |
|-----|--------|
| `↑` `↓` | Move between agents |
| `Tab` | Switch between Agents and Chat panels |
| `i` | Enter compose mode |
| `Enter` | Send message |
| `Esc` | Exit compose mode |
| `n` | Create new agent |
| `v` | View full agent details (model, prompt, config) |
| `e` | Edit agent |
| `d` | Delete agent |
| `q` | Quit |

After an agent responds, the dashboard automatically drops back into compose mode — no extra keypresses needed to continue the conversation.

---

## ⚙️ Config Reference

Config lives at `~/.agenta/config.toml`. The setup wizard writes this for you — edit manually only when needed.

```toml
# Core
ollama_url      = "http://localhost:11434"
default_model   = "qwen3:latest"
default_provider = "ollama"   # ollama | deepseek | openrouter | openai
log_level       = "info"
# timezone = "Asia/Kuala_Lumpur"  # optional — defaults to system timezone

# Storage
database_path = "~/.agenta/agenta.db"               # SQLite (default)
database_url  = "postgres://user:pass@localhost/db"  # Postgres (overrides SQLite)

# Daemon IPC socket
socket_path = "~/.agenta/agenta.sock"

# Model providers — api_key can be literal or "$ENV_VAR" (resolved from ~/.agenta/.env)
[providers.ollama]
url = "http://localhost:11434"

[providers.deepseek]
api_key = "$DEEPSEEK_API_KEY"

[providers.openrouter]
api_key = "$OPENROUTER_API_KEY"

[providers.openai]
api_key = "$OPENAI_API_KEY"

# Telegram — add one block per bot (or use: agenta setup telegram)
[[telegram_bots]]
name          = "my-bot"
token         = "$MY_BOT_TOKEN"
default_agent = "CORAL"

# REST API
api_port  = 8789
api_token = "replace-with-a-strong-token"
```

### Secrets

Secrets go in `~/.agenta/.env` — the daemon loads this file automatically on start:

```bash
# ~/.agenta/.env
DEEPSEEK_API_KEY=sk-...
OPENROUTER_API_KEY=sk-or-...
MY_BOT_TOKEN=123456:ABC...
```

Reference them in `config.toml` with a `$` prefix (e.g. `api_key = "$DEEPSEEK_API_KEY"`).

---

## ⌨️ Core Commands

```bash
agenta setup       # First-time setup wizard (provider, model, MIND, optional Telegram)
agenta setup telegram  # Add a Telegram bot to an existing install
agenta create      # Create an agent
agenta get         # Show agent details
agenta list        # List all agents
agenta update      # Update agent config
agenta delete      # Delete an agent
agenta run         # Run an agent once
agenta stop        # Stop a running agent
agenta logs        # View execution logs
agenta export      # Export agents to JSON/YAML
agenta import      # Import agents from file
agenta view        # View runtime data (executions, etc.)
agenta tool        # Manage tools (create/get/list/update/delete/run/logs)
agenta script      # Manage scripts (create/get/list/update/delete/run/logs)
agenta daemon      # start / stop / status / restart daemon
agenta upgrade     # Upgrade to the latest (or a specific) version
```

---

## ⚡ Common Workflows

### Create Your First Agent

```bash
agenta create \
  --name "CORAL" \
  --model "deepseek-chat" \
  --provider deepseek \
  --prompt "You are CORAL — a research companion. Given a topic, find relevant information, synthesise it clearly, and cite your reasoning."
```

All agents are **harnessed + memory enabled by default** — no extra flags needed.

### Run It

```bash
agenta run CORAL --input "What are the best local-first AI tools in 2025?" --wait
agenta logs CORAL --lines 50
```

### Update Prompt or Model

```bash
agenta update CORAL --prompt "You are a sharp research assistant. Bullet points only."
agenta update CORAL --model "gemma4:31b-cloud"
```

### Tune Parameters

```bash
agenta update CORAL --temperature 0.3
agenta update CORAL --max-tokens 8192
```

> **Heads up:** Models with extended thinking (e.g. `qwen3`) can run silently at low token limits. If your agent hangs without output, bump `--max-tokens` to `8192` or higher.

### Schedule a Daily Run

```bash
# Every morning at 8:00 AM local time
agenta update CORAL --mode scheduled --schedule "0 8 * * *"
```

> The scheduler uses your system timezone automatically. Override with `timezone = "Asia/Kuala_Lumpur"` in `config.toml` if needed.

### Back to Manual Only

```bash
agenta update CORAL --mode once --schedule ""
```

### Toggle Memory

Memory is **on by default** for all new agents. Toggle it on an existing agent:

```bash
agenta update CORAL --memory true
agenta update CORAL --memory false
```

### Use a Cloud Provider

Every agent can have its own provider override. Default resolves from `config.toml`.

```bash
# Create an agent using DeepSeek
agenta create \
  --name "WILL" \
  --model "deepseek-chat" \
  --provider deepseek \
  --prompt "You are WILL — a professional tech writer."

# Switch to OpenRouter
agenta update WILL --provider openrouter --model "anthropic/claude-3.5-sonnet"

# Switch back to local Ollama
agenta update WILL --provider ollama --model "qwen3:latest"
```

Provider resolution order: **agent `--provider`** → **`default_provider` in config.toml** → **ollama**

### Upgrade Agenta

```bash
# Upgrade to the latest release
agenta upgrade

# Upgrade to a specific version
agenta upgrade v1.1.0
```

The daemon is stopped automatically before upgrading and must be restarted after:

```bash
agenta daemon stop && agenta upgrade && agenta daemon start
```

### Export / Import Agents

```bash
# Back up everything
agenta export all -o ~/.agenta/exports/backup.json

# Back up one agent
agenta export CORAL -o coral.json

# Import (skip duplicates)
agenta import -i backup.json

# Import and overwrite
agenta import -i backup.json --force
```

> **Auto-backup:** The daemon exports all agents to `~/.agenta/exports/backup_YYYYMMDD_HHMMSS.json` on every start, keeping the last 14 backups.

---

## 🧠 Harness Mode (All Agents)

Every agent — without exception — runs in **harness mode**. There is no "simple" agent. The moment you create one, it gets multi-step reasoning, iterative tool use, built-in file tools, and memory. You don't opt in; you opt out if you need to.

The loop uses a proper chat message history — not a growing flat prompt. Each turn the agent's response and any tool results are fed back as conversation messages, exactly how the model was trained to reason:

```
system  → instructions + tool list + memory
user    → your task
assistant → TOOL_CALL: {"tool": "read_file", ...}
user    → TOOL_RESULT: <file contents>
assistant → TOOL_CALL: {"tool": "write_file", ...}
user    → TOOL_RESULT: Written 412 bytes to ~/out/result.md
assistant → TASK_COMPLETE: Done. Summary written to ~/out/result.md.
```

The loop exits when the agent writes `TASK_COMPLETE:` or hits the iteration limit.

### Iteration Limit

```bash
agenta update CORAL --deep-iterations 20   # default is 10
```

### Built-in Tools

Available to every agent with zero setup — no tool registration, no handlers, no scripts:

| Tool | Description |
|------|-------------|
| `spawn_agent` | Spawn a sub-agent and get its output |
| `read_file` | Read a file from disk |
| `write_file` | Write content to a file |
| `list_files` | List files matching a glob pattern |

The agent calls them the same way as any other tool:

```
TOOL_CALL: {"tool": "read_file", "parameters": {"path": "~/reports/summary.txt"}}
TOOL_CALL: {"tool": "write_file", "parameters": {"path": "~/out/result.md", "content": "..."}}
TOOL_CALL: {"tool": "list_files", "parameters": {"pattern": "~/reports/*.md"}}
```

Tool output is capped at 8,000 characters to protect the context window.

### External Tools

Attach shell-script tools to give agents web search, API access, or anything else:

```bash
agenta tool create \
  --name web-search \
  --description "Search the web for current information" \
  --parameters '{"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}' \
  --handler "~/.agenta/tools/tavily_search.sh"

agenta update CORAL --tools ~/.agenta/tools/search_tools.json
```

---

## 🪄 Sub-Agent Spawning

Agents can delegate work to sub-agents at runtime.

### Ephemeral Sub-Agent

Spins up a throwaway agent, gets its answer, and it disappears — nothing saved to the database:

```
TOOL_CALL: {"tool": "spawn_agent", "parameters": {
  "role": "You are a financial analyst. Be precise, cite numbers.",
  "input": "Summarise the latest earnings report for NVIDIA.",
  "model": "gemma4:31b-cloud"
}}
```

| Parameter | Required | Description |
|-----------|----------|-------------|
| `role`    | Yes      | System prompt for the sub-agent |
| `input`   | Yes      | The task to hand off |
| `model`   | No       | Model override (defaults to parent's model) |

### Named Agent Delegation

Delegate to an existing agent by name — it runs with its own model, memory, and prompt:

```
TOOL_CALL: {"tool": "spawn_agent", "parameters": {
  "name": "SENTRI",
  "input": "Check disk usage on the production server."
}}
```

| Parameter | Required | Description |
|-----------|----------|-------------|
| `name`    | Yes      | Name of an existing agent in the database |
| `input`   | Yes      | The task to hand off |

### Progress Notifications

When a sub-agent is spawned, a notification is sent to the caller (e.g. your Telegram chat):

```bash
# Customise the message ({task} is replaced at runtime)
agenta update CORAL --spawn-message "🔍 Delegating to SENTRI: {task}"

# Reset to default
agenta update CORAL --spawn-message ""
```

Default: `⚙️ Spawning sub-agent: <task>`

---

## 💬 Telegram Integration

Chat with your agents directly from Telegram. No webhook, no public URL, no tunnel — just long polling.

### Quick Setup

```bash
agenta setup telegram
```

This wizard asks for your bot token, shows your agent list to pick a default handler, and writes the config. Run it once per bot.

### Manual Setup

**1. Create a bot** via [@BotFather](https://t.me/BotFather) and copy the token.

**2. Add the token to `~/.agenta/.env`:**

```bash
CORAL_BOT_TOKEN=123456:ABC...
```

**3. Register in `config.toml`:**

```toml
[[telegram_bots]]
name          = "coral"
token         = "$CORAL_BOT_TOKEN"
default_agent = "CORAL"

[[telegram_bots]]
name          = "sentri"
token         = "$SENTRI_BOT_TOKEN"
default_agent = "SENTRI"
```

Each bot runs its own polling loop. Scale to as many bots as you want.

**4. Restart the daemon:**

```bash
agenta daemon restart
```

### Message Routing

- Default: all messages go to `default_agent`
- Override per message: `/agent <agent-name> your message here`

### Troubleshooting

`409 Conflict` in logs means a webhook is still registered. Clear it:

```bash
curl "https://api.telegram.org/bot<TOKEN>/deleteWebhook"
```

---

## 🤖 MIND — System Agent

`agenta setup` creates **MIND** automatically — a privileged system agent for managing the agenta ecosystem itself. MIND is hidden from `agenta list` and protected from deletion.

MIND is designed to generate shell scripts, create tools, and assist with configuration. Ask it things like:

```
Write a tool that fetches the top 5 Hacker News headlines and returns them as JSON.
```

Interact with MIND the same as any agent:

```bash
agenta run MIND --input "Create a tool that monitors CPU usage and alerts when above 80%." --wait
```

---

## 🌐 REST API + Swagger

```toml
api_port  = 8789
api_token = "replace-with-a-strong-token"  # optional
```

| Endpoint | URL |
|----------|-----|
| API base | `http://127.0.0.1:8789/api` |
| Swagger UI | `http://127.0.0.1:8789/swagger-ui` |
| OpenAPI JSON | `http://127.0.0.1:8789/api-doc/openapi.json` |

Auth (when `api_token` is set):

```bash
curl -H "Authorization: Bearer $AGENTA_API_TOKEN" \
  http://127.0.0.1:8789/api/health
```

Also accepts `x-api-key: <token>`.

---

## 🗄️ Database

### SQLite (Default)

No setup needed. Agenta creates the database automatically at `database_path`.

### Postgres

```toml
database_url = "postgres://postgres:password@localhost:5432/mydb"
```

When `database_url` is set, Agenta uses Postgres and ignores `database_path`.

---

## 🔧 Troubleshooting

### Daemon won't start

```bash
agenta daemon status
pkill -f agenta-daemon || true
agenta daemon start
```

### `Address already in use`

A stale socket from a crashed daemon. Fix:

```bash
pkill -f agenta-daemon || true
agenta daemon start
```

### Telegram `409 Conflict`

A previously registered webhook is blocking polling:

```bash
curl "https://api.telegram.org/bot<TOKEN>/deleteWebhook"
```

### Agent hangs silently (no output)

Usually a low `max_tokens` limit with a thinking model. Bump it:

```bash
agenta update <agent-name> --max-tokens 8192
```

### Swagger shows stale docs

Hard refresh the browser tab or reopen the Swagger URL after daemon restart.

---

## 🏗️ Architecture

```
┌────────────────────────────────────────────────────────────────────────────────┐
│                               AGENTA PLATFORM                                  │
│                                                                                │
│  ┌──────────────────────────────────────────────────────────────────────────┐  │
│  │                              ENTRY POINTS                                │  │
│  │                                                                          │  │
│  │  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  │  │
│  │  │  CLI / TUI   │  │   Telegram   │  │   REST API   │  │  Scheduler   │  │  │
│  │  │              │  │              │  │              │  │              │  │  │
│  │  │ agenta run   │  │  multi-bot   │  │  :8789       │  │ 0 8 * * *    │  │  │
│  │  │ agenta (tui) │  │  long-poll   │  │  + Swagger   │  │  triggers    │  │  │
│  │  └──────────────┘  └──────────────┘  └──────────────┘  └──────────────┘  │  │
│  └──────────────────────────────────────────────────────────────────────────┘  │
│                                       │                                        │
│                                       ▼                                        │
│  ┌──────────────────────────────────────────────────────────────────────────┐  │
│  │                              DAEMON CORE                                 │  │
│  │                                                                          │  │
│  │  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  │  │
│  │  │ Deep Harness │  │  Tool Loop   │  │  Sub-Agents  │  │   Memory     │  │  │
│  │  │              │  │              │  │              │  │              │  │  │
│  │  │ chat history │  │ built-in +   │  │ ephemeral or │  │ last 6 runs  │  │  │
│  │  │ tool→result  │  │ shell tools  │  │ named agents │  │ as context   │  │  │
│  │  └──────────────┘  └──────────────┘  └──────────────┘  └──────────────┘  │  │
│  └──────────────────────────────────────────────────────────────────────────┘  │
│                                       │                                        │
│                                       ▼                                        │
│  ┌──────────────────────────────────────────────────────────────────────────┐  │
│  │                               BACKENDS                                   │  │
│  │                                                                          │  │
│  │  ┌──────────────────────────────────┐  ┌──────────┐  ┌───────────────┐   │  │
│  │  │        Model Backend (pluggable) │  │ Storage  │  │  Shell Tools  │   │  │
│  │  │                                  │  │          │  │               │   │  │
│  │  │  Ollama · DeepSeek · OpenRouter  │  │ SQLite   │  │ ~/.agenta/    │   │  │
│  │  │  OpenAI · any OpenAI-compat API  │  │ Postgres │  │ tools/        │   │  │
│  │  └──────────────────────────────────┘  └──────────┘  └───────────────┘   │  │
│  └──────────────────────────────────────────────────────────────────────────┘  │
└────────────────────────────────────────────────────────────────────────────────┘
```

---

## 📝 Notes

- The daemon must be running for CLI operations that use socket RPC.
- Scheduling, triggers, Telegram, and the REST API all run inside the daemon process.
- `agenta daemon status` is the source of truth for daemon health.
- All agents run in deep harness mode with memory by default — no `--deep` or `--memory` flag needed.
- Built-in tools (`read_file`, `write_file`, `list_files`, `spawn_agent`) require no registration.
- Sub-agents spawned with `role` are ephemeral — not saved to the database, not listable.
- Sub-agents spawned with `name` delegate to a real existing agent.
- Tools live in `~/.agenta/tools/` — decoupled from the repo, safe across upgrades.
- MIND is a protected system agent — hidden from `agenta list`, cannot be deleted.
