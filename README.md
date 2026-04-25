<p align="center">
  <img src="assets/agenta-logo.svg" width="220" alt="Agenta Logo">
</p>

<h1 align="center">Agenta</h1>

<p align="center">
  <strong>Local-first AI agent platform.</strong> Build, run, schedule, and operate autonomous agents from one CLI + daemon with triggers, tools, deep agents, sub-agent spawning, REST API, and Telegram integration. Built with <strong>Rust</strong>.
</p>

---

## What You Get

- Local agent management (`create`, `update`, `run`, `logs`, `list`)
- Daemon runtime with scheduling and triggers
- **Deep agents** — multi-step reasoning with iterative tool use
- **Sub-agent spawning** — deep agents can deploy ephemeral sub-agents at runtime
- Agent memory — recall past executions as context
- Export / import agents (with auto-backup on every daemon start)
- Optional Postgres backend (SQLite by default)
- Optional REST API + Swagger
- **Multi-bot Telegram long-polling gateway** (one bot per agent, no webhook/tunnel needed)

---

## First-Time Setup (Recommended Path)

### 1. Prerequisites

- Ollama installed and running at `http://localhost:11434`
- At least one model pulled (example below)

```bash
ollama pull qwen3:latest
ollama ps
```

### 2. Install Agenta

Option A: From source (current repo)

```bash
cargo install --path . --force
```

Option B: Install directly from GitHub raw

```bash
curl -fsSL https://raw.githubusercontent.com/warifmust/agenta/main/install.sh | bash
```

Local installer script in this repo:

```bash
./install.sh
```

Installer env vars (optional):

```bash
AGENTA_REPO="arifmustaffa/agenta"   # GitHub repo owner/name
AGENTA_VERSION="latest"             # release tag or "latest"
AGENTA_INSTALL_DIR="$HOME/.local/bin"
./install.sh
```

### 3. Verify CLI

```bash
agenta --help
agenta daemon --help
```

### 4. Configure Agenta

Agenta config path:

- macOS: `~/Library/Application Support/agenta/config.toml`
- Linux: `~/.config/agenta/config.toml`

Minimal config example:

```toml
ollama_url = "http://localhost:11434"
default_model = "qwen3:latest"
```

All supported `config.toml` keys:

```toml
# Core
ollama_url = "http://localhost:11434"
default_model = "qwen3:latest"
log_level = "info"

# Storage
# SQLite path (used when database_url is not set)
database_path = "/Users/<you>/.agenta/agenta.db"
# Optional Postgres DSN (takes precedence over database_path)
database_url = "postgres://postgres:<password>@localhost:5432/postgres"

# Daemon IPC socket
socket_path = "/Users/<you>/.agenta/agenta.sock"

# Legacy single Telegram bot (still supported)
telegram_bot_token = "<telegram-bot-token>"
telegram_default_agent = "travel-guide"

# Multi-bot Telegram polling (one entry per bot)
[[telegram_bots]]
name = "my-bot"
token = "$MY_BOT_TOKEN"          # reads from ~/.agenta/.env
default_agent = "travel-guide"

# REST API
api_port = 8789
api_token = "replace-with-a-strong-token"
```

Notes:
- If `database_url` is set, Agenta uses Postgres.
- If `database_url` is not set, Agenta uses SQLite at `database_path`.
- `telegram_*` fields are optional — only needed for Telegram chat integration.
- `api_token` is optional; if set, API endpoints require auth.

### 5. Environment Variables

Secrets (API keys, bot tokens) go in `~/.agenta/.env`. The daemon loads this file automatically on startup:

```bash
# ~/.agenta/.env
TELEGRAM_BOT_TOKEN=<token>
TELEGRAM_CHAT_ID=<chat-id>
TAVILY_API_KEY=<key>
MY_CUSTOM_BOT_TOKEN=<token>
```

In `config.toml`, reference env vars with a `$` prefix:

```toml
[[telegram_bots]]
token = "$MY_CUSTOM_BOT_TOKEN"
default_agent = "my-agent"
```

### 6. Start Daemon

```bash
agenta daemon start
agenta daemon status
```

### 7. Create Your First Agent

```bash
agenta create \
  --name "travel-guide" \
  --model "qwen3:latest" \
  --prompt "You are a practical travel assistant. Plain text only."
```

### 8. Run It

```bash
agenta run travel-guide --input "Plan a 2D1N trip to Bangkok" --wait
agenta logs travel-guide --lines 50
```

---

## Core Commands

```bash
agenta create      # Create agent
agenta get         # Show agent details
agenta list        # List agents
agenta update      # Update agent config
agenta delete      # Delete agent
agenta run         # Run once
agenta stop        # Stop running agent
agenta logs        # Execution logs
agenta export      # Export agents to JSON/YAML
agenta import      # Import agents from file
agenta view        # View runtime data (e.g., executions)
agenta tool        # Tool lifecycle (create/get/list/update/delete/run/logs)
agenta script      # Script lifecycle (create/get/list/update/delete/run/logs)
agenta daemon      # start/stop/status/restart daemon
```

---

## Common Workflows

### Update Prompt / Model

```bash
agenta update travel-guide --prompt "New system prompt"
agenta update travel-guide --model "qwen3:latest"
```

### Tune Model Parameters

```bash
agenta update travel-guide --temperature 0.5
agenta update travel-guide --max-tokens 8192
```

> **Tip:** Models with extended thinking (e.g. `qwen3`) may run silently for a long time with the default token limit. Increase `--max-tokens` (e.g. `8192`) if your agent hangs without producing output.

### Schedule Daily Run (10:00)

```bash
agenta update travel-guide --mode scheduled --schedule "0 10 * * *"
```

### Switch Back to Manual Only

```bash
agenta update travel-guide --mode once --schedule ""
```

### Enable Agent Memory

Agents with memory enabled inject their last 6 past outputs as context on every run — useful for chat-style or recurring task agents.

```bash
# Enable memory on create
agenta create --name "my-agent" --model "qwen3:latest" --prompt "..." --memory

# Enable/disable on existing agent
agenta update my-agent --memory true
agenta update my-agent --memory false
```

### Export / Import Agents

```bash
# Export all agents
agenta export all -o ~/.agenta/exports/backup.json

# Export a single agent
agenta export my-agent -o my-agent.json

# Import (skip duplicates)
agenta import -i backup.json

# Import and overwrite existing agents
agenta import -i backup.json --force
```

> **Auto-backup:** Every time the daemon starts, it automatically exports all agents to `~/.agenta/exports/backup_YYYYMMDD_HHMMSS.json` and keeps the last 14 backups.

### Attach Tools

```bash
agenta update travel-guide --tools tools/echo.json,tools/another.yaml
```

### Manage First-Class Tools

Create tool (script scaffolded to `~/.agenta/tools/<name>.sh`):

```bash
agenta tool create \
  --name web-fetch \
  --description "Fetch web content via custom script" \
  --parameters '{"type":"object","properties":{"url":{"type":"string"}},"required":["url"]}'
```

Provide a custom handler instead of scaffolding:

```bash
agenta tool create \
  --name web-fetch \
  --description "Fetch web content via custom script" \
  --parameters '{"type":"object","properties":{"url":{"type":"string"}},"required":["url"]}' \
  --handler "/Users/you/bin/web_fetch_tool"
```

Scaffold starter script automatically:

```bash
agenta tool create \
  --name web_fetch_readonly \
  --description "Read-only web fetch tool" \
  --parameters '{"type":"object","properties":{"url":{"type":"string"}},"required":["url"]}' \
  --scaffold
```

List tools:

```bash
agenta tool list
```

Get tool details:

```bash
agenta tool get web-fetch
```

Run tool manually:

```bash
agenta tool run web-fetch --input '{"url":"https://www.tourismthailand.org"}' --wait
```

View tool logs:

```bash
agenta tool logs web-fetch --lines 50
```

View agent executions:

```bash
agenta view executions
agenta view executions --limit 200
```

Update tool:

```bash
agenta tool update web-fetch --enabled false
```

Delete tool:

```bash
agenta tool delete web-fetch
```

---

## Deep Agents

Deep agents run in a multi-step reasoning loop — they can call tools, evaluate results, and iterate until they reach a conclusion or hit the iteration limit.

### Create a Deep Agent

```bash
agenta create \
  --name "researcher" \
  --model "deepseek-v3.1:671b-cloud" \
  --prompt "You are a research agent. Use available tools to answer questions thoroughly." \
  --deep \
  --deep-iterations 10
```

### How It Works

Each iteration the agent can:
1. Call a tool via `TOOL_CALL: {"tool": "<name>", "parameters": {...}}`
2. Observe the result and decide next action
3. Finish by writing `TASK_COMPLETE: <final answer>`

The loop exits when:
- The agent writes `TASK_COMPLETE:`
- A stop condition is matched
- The iteration limit is reached

### Tool Definition for Deep Agents

Define tools in a JSON file and attach to a deep agent:

```json
[
  {
    "name": "tavily_search",
    "description": "Search the web. Parameters: {\"query\": \"<query>\", \"max_results\": 5}",
    "parameters": {
      "type": "object",
      "properties": {
        "query": { "type": "string" },
        "max_results": { "type": "integer" }
      },
      "required": ["query"]
    },
    "handler": "/usr/bin/env bash ~/.agenta/tools/tavily_search.sh"
  }
]
```

```bash
agenta update researcher --tools ~/.agenta/tools/my_tools.json
```

---

## Sub-Agent Spawning

Deep agents can spawn ephemeral sub-agents at runtime using the built-in `spawn_agent` tool. Sub-agents run synchronously, return their output to the parent, and are never persisted to the database.

### How to Use

In your deep agent's system prompt, instruct it to call `spawn_agent`:

```
TOOL_CALL: {"tool": "spawn_agent", "parameters": {
  "role": "You are a research analyst specialising in regulatory affairs.",
  "input": "What are the current EU AI Act regulations affecting LLM providers?",
  "model": "deepseek-v3.1:671b-cloud"
}}
```

Parameters:

| Parameter | Required | Description |
|-----------|----------|-------------|
| `role`    | Yes      | System prompt for the sub-agent |
| `input`   | Yes      | The task or question to answer |
| `model`   | No       | Model to use (defaults to parent's model) |

### Sub-Agent Spawn Notifications

When a deep agent spawns a sub-agent, a progress notification is sent to the caller (e.g. Telegram chat). The message is configurable per agent:

```bash
# Set a custom notification message ({task} is replaced with the actual task)
agenta update my-agent --spawn-message "🤖 Deploying sub-agent: {task}"

# Clear custom message (reverts to generic default)
agenta update my-agent --spawn-message ""
```

Default message (used when no custom message is set):

```
⚙️ Spawning sub-agent: <task>
```

### Built-in Tools

Built-in tools are available to all deep agents without any configuration:

| Tool | Description |
|------|-------------|
| `spawn_agent` | Spawn an ephemeral sub-agent, wait for its output, and return the result to the parent agent |

---

## Telegram Integration

No public URL or webhook setup needed. The daemon polls Telegram for new messages automatically using long polling.

### Setup

**1. Create a bot** via [@BotFather](https://t.me/BotFather) on Telegram and copy the token.

**2. Add your bot token to `~/.agenta/.env`:**

```bash
MY_BOT_TOKEN=<your-bot-token>
```

**3. Add one or more bots to `config.toml`:**

```toml
[[telegram_bots]]
name = "assistant"
token = "$MY_BOT_TOKEN"           # reads from ~/.agenta/.env
default_agent = "travel-guide"

[[telegram_bots]]
name = "researcher"
token = "$RESEARCH_BOT_TOKEN"
default_agent = "my-research-agent"
```

Each bot entry gets its own polling loop. Messages are routed to the configured `default_agent`.

**4. Restart the daemon:**

```bash
agenta daemon stop && agenta daemon start
```

### Routing

- Default: messages go to `default_agent`
- Override: send `/agent <agent-name> <message>` to route to a specific agent

### Troubleshooting

If the daemon logs a `409 Conflict` error, a webhook is registered on the bot. Delete it first:

```bash
curl "https://api.telegram.org/bot<TOKEN>/deleteWebhook"
```

---

## REST API + Swagger

Setup:

```toml
api_port = 8789
api_token = "replace-with-a-strong-token" # optional
```

Start daemon (this also starts REST API + Swagger server):

```bash
agenta daemon start
```

Verify:

```bash
agenta daemon status
```

Endpoints:

- API base: `http://127.0.0.1:8789/api`
- Swagger UI: `http://127.0.0.1:8789/swagger-ui`
- OpenAPI JSON: `http://127.0.0.1:8789/api-doc/openapi.json`

Auth (if `api_token` is set):

- `Authorization: Bearer <token>`
- `x-api-key: <token>`

Example:

```bash
curl -H "Authorization: Bearer $AGENTA_API_TOKEN" \
  http://127.0.0.1:8789/api/health
```

---

## Database Configuration

### SQLite (Default)

No extra setup needed; Agenta uses local `database_path`.

### Postgres (Optional)

```toml
database_url = "postgres://postgres:<password>@localhost:5432/postgres"
```

If `database_url` is set, daemon uses Postgres. If not set, daemon uses SQLite.

---

## Troubleshooting

### `Daemon is not running`

```bash
agenta daemon start
agenta daemon status
```

### `Address already in use`

You likely have another daemon instance.

```bash
pkill -f agenta-daemon || true
agenta daemon start
```

### Telegram polling conflict (`409 Conflict`)

If you previously registered a webhook on the bot, polling will fail with a 409 error. Delete the webhook first:

```bash
curl "https://api.telegram.org/bot<TOKEN>/deleteWebhook"
```

### Swagger resolver errors / stale docs

Hard refresh browser or reopen Swagger URL after daemon restart.

---

## Notes

- Daemon must be running for CLI operations that use socket RPC.
- Scheduling, triggers, chat gateway, and REST API all run inside daemon process.
- Use `agenta daemon status` as source of truth for daemon health.
- Sub-agents spawned by deep agents are ephemeral — they are not saved to the database and cannot be listed or queried.
- Tools live in `~/.agenta/tools/` — decoupled from the repo so they persist across upgrades.
