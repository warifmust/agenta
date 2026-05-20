<p align="center">
  <img src="assets/agenta-logo-2.png" width="240" alt="Agenta Logo">
</p>

<h1 align="center">Agenta</h1>

<p align="center">
  <strong>Define it. Deploy it. Forget it.</strong>
</p>

<p align="center">
  Local-first agent runtime for autonomous pipelines. Ollama by default, swap to DeepSeek, OpenRouter, or OpenAI per agent, no re-architecture needed. Tools, deep reasoning, sub-agent spawning, Telegram, REST API. No vendor lock-in. No subscriptions. Just control. Powered by <strong>Rust</strong>.
</p>

---

## ✨ What You Get

- 🤖 **Agent management** — `create`, `update`, `run`, `logs`, `list` from the CLI
- ⏰ **Scheduling** — cron-based scheduling baked into the daemon
- 🧠 **Deep agents** — multi-step reasoning with iterative tool use
- 🪄 **Sub-agent spawning** — agents can spin up other agents at runtime
- 💬 **Telegram integration** — multiple bots, one daemon, no webhook or tunnel needed
- 🧵 **Agent memory** — inject past outputs as context on every run
- 📦 **Export / import** — backup agents as JSON/YAML, auto-backup on every daemon start
- 🗄️ **SQLite by default**, Postgres optional
- 🌐 **REST API + Swagger UI** — built-in, no extra setup
- 🔌 **Pluggable model backends** — Ollama (local), DeepSeek, OpenRouter, OpenAI — per-agent override
- 🏠 **Fully self-hosted** — runs on your laptop, a cheap VPS, or a Raspberry Pi

---

## 🚀 First-Time Setup

### 1. Prerequisites

- [Ollama](https://ollama.com) installed and running
- At least one model pulled

```bash
ollama pull gemma4:e4b # or any model from ollama
ollama ps
```

### 2. Install Agenta

**From GitHub (recommended):**

```bash
curl -fsSL https://raw.githubusercontent.com/warifmust/agenta/main/install.sh | bash
```

**From source:**

Requires Rust. If `cargo` is not found, install it via [rustup](https://rustup.rs) (works on macOS and Linux — prefer this over `brew` or `apt` which ship outdated versions):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh  # macOS and Linux
source $HOME/.cargo/env
```

Then build and install:
```bash
cargo install --path . --force
```

**Custom install options:**

```bash
AGENTA_REPO="warifmust/agenta"      # GitHub repo
AGENTA_VERSION="latest"             # release tag or "latest"
AGENTA_INSTALL_DIR="$HOME/.local/bin"
./install.sh
```

### 3. Verify

```bash
agenta --help
agenta daemon --help
```

### 4. Configure

Config lives at `~/.agenta/config.toml`.

Minimal config:

```toml
ollama_url = "http://localhost:11434"
default_model = "gemma4:e4b"
```

Full config reference:

```toml
# Core
ollama_url = "http://localhost:11434"
default_model = "gemma4:e4b"
default_provider = "ollama"   # ollama | deepseek | openrouter | openai
log_level = "info"
# timezone = "Asia/Kuala_Lumpur"  # optional — defaults to system timezone

# Storage
database_path = "~/.agenta/agenta.db"              # SQLite (default)
database_url  = "postgres://user:pass@localhost/db" # Postgres (overrides SQLite)

# Daemon IPC socket
socket_path = "~/.agenta/agenta.sock"

# Model providers — api_key can be a literal or "$ENV_VAR" (resolved from ~/.agenta/.env)
[providers.ollama]
# url = "http://localhost:11434"   # overrides ollama_url if set

[providers.deepseek]
api_key = "$DEEPSEEK_API_KEY"
# url = "https://api.deepseek.com/v1"   # default

[providers.openrouter]
api_key = "$OPENROUTER_API_KEY"
# url = "https://openrouter.ai/api/v1"  # default

[providers.openai]
api_key = "$OPENAI_API_KEY"
# url = "https://api.openai.com/v1"     # default

# Telegram — multiple bots supported
[[telegram_bots]]
name = "my-bot"
token = "$MY_BOT_TOKEN"       # resolved from ~/.agenta/.env
default_agent = "my-agent"

# REST API
api_port  = 8789
api_token = "replace-with-a-strong-token"
```

### 5. Secrets

Secrets go in `~/.agenta/.env` — the daemon loads this automatically:

```bash
# ~/.agenta/.env
MY_BOT_TOKEN=your-telegram-bot-token
TELEGRAM_CHAT_ID=your-chat-id
TAVILY_API_KEY=your-tavily-key
```

Reference them in `config.toml` with a `$` prefix:

```toml
[[telegram_bots]]
token = "$MY_BOT_TOKEN"
default_agent = "my-agent"
```

### 6. Start the Daemon

```bash
agenta daemon start
agenta daemon status
```

### 7. Create Your First Agent

Let's build a **morning briefing agent** that summarises the day ahead:

```bash
agenta create \
  --name "morning-brief" \
  --model "gemma4:e4b" \
  --prompt "You are a sharp, concise personal assistant. Given a topic or question, respond with clear, useful insights. No filler, no fluff."
```

### 8. Run It

```bash
agenta run morning-brief --input "What should I know about AI news this week?" --wait
agenta logs morning-brief --lines 50
```

---

## ⌨️ Core Commands

```bash
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
agenta upgrade     # upgrade agenta to the latest (or a specific) version
```

---

## ⚡ Common Workflows

### Update Prompt or Model

```bash
agenta update morning-brief --prompt "You are a concise assistant. Bullet points only."
agenta update morning-brief --model "gemma4:e4b"
```

### Tune Parameters

```bash
agenta update morning-brief --temperature 0.3
agenta update morning-brief --max-tokens 8192
```

> **Heads up:** Models with extended thinking (e.g. `qwen3`) can run silently for a while at low token limits. If your agent hangs without output, bump `--max-tokens` to `8192` or higher.

### Schedule a Daily Run

```bash
# Every morning at 8:00 AM (local time — no UTC conversion needed)
agenta update morning-brief --mode scheduled --schedule "0 8 * * *"
```

> The scheduler uses your system timezone automatically. `8am` means `8am` on your machine. Override with `timezone = "Asia/Kuala_Lumpur"` in `~/.agenta/config.toml` if needed.

### Back to Manual Only

```bash
agenta update morning-brief --mode once --schedule ""
```

### Enable Agent Memory

Memory injects the last 6 outputs as context — great for chat-style or recurring agents.

```bash
# On create
agenta create --name "standup-bot" --model "gemma4:e4b" --prompt "..." --memory

# On existing agent
agenta update standup-bot --memory true
agenta update standup-bot --memory false
```

### Use a Cloud Provider

Every agent can have its own provider override. Default is Ollama.

```bash
# Create an agent using DeepSeek
agenta create \
  --name "writer" \
  --model "deepseek-chat" \
  --provider deepseek \
  --prompt "You are a professional tech writer."

# Switch an existing agent to OpenRouter
agenta update my-agent --provider openrouter --model "anthropic/claude-3.5-sonnet"

# Switch back to local Ollama
agenta update my-agent --provider ollama --model "gemma4:e4b"
```

Provider resolution order: **agent `--provider`** → **`default_provider` in config.toml** → **ollama**

Add API keys to `~/.agenta/.env`:

```bash
DEEPSEEK_API_KEY=sk-...
OPENROUTER_API_KEY=sk-or-...
OPENAI_API_KEY=sk-...
```

### Upgrade Agenta

```bash
# Upgrade to the latest release
agenta upgrade

# Upgrade to a specific version
agenta upgrade v1.0.7
```

The daemon is stopped automatically before upgrading and must be restarted after:

```bash
agenta daemon stop
agenta upgrade
agenta daemon start
```

### Export / Import Agents

```bash
# Back up everything
agenta export all -o ~/.agenta/exports/backup.json

# Back up one agent
agenta export morning-brief -o morning-brief.json

# Import (skip duplicates)
agenta import -i backup.json

# Import and overwrite
agenta import -i backup.json --force
```

> **Auto-backup:** The daemon automatically exports all agents to `~/.agenta/exports/backup_YYYYMMDD_HHMMSS.json` on every start, keeping the last 14 backups.

---

## 🧰 Tools

Tools let agents call external scripts — web search, file reads, API calls, anything a shell script can do.

### Attach Tools to an Agent

```bash
agenta update my-agent --tools ~/.agenta/tools/my_tools.json
```

### Create a Tool

```bash
agenta tool create \
  --name web-search \
  --description "Search the web for current information" \
  --parameters '{"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}'
```

With a custom handler:

```bash
agenta tool create \
  --name web-search \
  --description "Search the web for current information" \
  --parameters '{"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}' \
  --handler "/Users/you/bin/tavily_search.sh"
```

Auto-scaffold a starter script:

```bash
agenta tool create \
  --name web-search \
  --description "Search the web" \
  --parameters '{"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}' \
  --scaffold
```

### Manage Tools

```bash
agenta tool list
agenta tool get web-search
agenta tool run web-search --input '{"query":"Rust vs Go performance 2025"}' --wait
agenta tool logs web-search --lines 50
agenta tool update web-search --enabled false
agenta tool delete web-search
```

### View Agent Executions

```bash
agenta view executions
agenta view executions --limit 200
```

---

## 🧠 Deep Agents

Deep agents don't just generate one response, they think, act, observe, and iterate. Perfect for research, multi-step tasks, or anything that needs more than one shot.

### Create a Deep Agent

```bash
agenta create \
  --name "deal-hunter" \
  --model "gemma4:e4b" \
  --prompt "You are a sharp deal-finding agent. Search for the best prices, compare options, and give a clear recommendation with reasoning." \
  --deep \
  --deep-iterations 10
```

### How It Works

Each iteration the agent can:
1. Call a tool → `TOOL_CALL: {"tool": "<name>", "parameters": {...}}`
2. Observe the result and decide what to do next
3. Conclude with → `TASK_COMPLETE: <final answer>`

The loop exits when:
- The agent writes `TASK_COMPLETE:`
- A stop condition is matched
- The iteration limit is reached

### Tool Definition

Define tools in a JSON file:

```json
[
  {
    "name": "web_search",
    "description": "Search the web for current information.",
    "parameters": {
      "type": "object",
      "properties": {
        "query": { "type": "string" },
        "max_results": { "type": "integer" }
      },
      "required": ["query"]
    },
    "handler": "/Users/you/.agenta/tools/tavily_search.sh"
  }
]
```

```bash
agenta update deal-hunter --tools ~/.agenta/tools/search_tools.json
```

---

## 🪄 Sub-Agent Spawning

Deep agents can spin up ephemeral sub-agents at runtime, like delegating work to a specialist. Sub-agents run, return their answer, and disappear. Nothing is saved to the database.

### How to Use

Instruct your agent to call `spawn_agent` in its prompt:

```
TOOL_CALL: {"tool": "spawn_agent", "parameters": {
  "role": "You are a financial analyst. Be precise, cite numbers.",
  "input": "Summarise the latest earnings report for NVIDIA.",
  "model": "gemma4:e4b"
}}
```

| Parameter | Required | Description |
|-----------|----------|-------------|
| `role`    | Yes      | System prompt for the sub-agent |
| `input`   | Yes      | The task or question to hand off |
| `model`   | No       | Model override (defaults to parent's model) |

### Progress Notifications

When a sub-agent is spawned, a notification is sent to the caller (e.g. your Telegram chat):

```bash
# Customise the message ({task} is replaced at runtime)
agenta update deal-hunter --spawn-message "🔍 Delegating to specialist: {task}"

# Reset to default
agenta update deal-hunter --spawn-message ""
```

Default: `⚙️ Spawning sub-agent: <task>`

### Built-in Tools

Available to all deep agents, no setup needed:

| Tool | Description |
|------|-------------|
| `spawn_agent` | Spawn an ephemeral sub-agent and get its output |

---

## 💬 Telegram Integration

Chat with your agents directly from Telegram. No webhook, no public URL, no tunnel — just long polling.

### Setup

**1. Create a bot** via [@BotFather](https://t.me/BotFather) and copy the token.

**2. Add the token to `~/.agenta/.env`:**

```bash
MY_BOT_TOKEN=your-bot-token
```

**3. Register bots in `config.toml`:**

```toml
[[telegram_bots]]
name = "assistant"
token = "$MY_BOT_TOKEN"
default_agent = "morning-brief"

[[telegram_bots]]
name = "researcher"
token = "$RESEARCH_BOT_TOKEN"
default_agent = "deal-hunter"
```

Each bot runs its own polling loop. Scale to as many bots as you want.

**4. Restart the daemon:**

```bash
agenta daemon stop && agenta daemon start
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

## 🌐 REST API + Swagger

```toml
api_port  = 8789
api_token = "replace-with-a-strong-token"  # optional
```

```bash
agenta daemon start
agenta daemon status
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

When `database_url` is set, Agenta uses Postgres. When it's not, SQLite is used.

---

## 🔧 Troubleshooting

### Daemon won't start

```bash
agenta daemon start
agenta daemon status
```

### `Address already in use`

Kill the stale daemon process and restart:

```bash
pkill -f agenta-daemon || true
agenta daemon start
```

### Telegram `409 Conflict`

A previously registered webhook is blocking polling:

```bash
curl "https://api.telegram.org/bot<TOKEN>/deleteWebhook"
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
│  │  │     CLI      │  │   Telegram   │  │   REST API   │  │  Scheduler   │  │  │
│  │  │              │  │              │  │              │  │              │  │  │
│  │  │ agenta run   │  │  multi-bot   │  │  :8789       │  │ 0 8 * * *    │  │  │
│  │  │ agenta logs  │  │  long-poll   │  │  + Swagger   │  │  triggers    │  │  │
│  │  └──────────────┘  └──────────────┘  └──────────────┘  └──────────────┘  │  │
│  └──────────────────────────────────────────────────────────────────────────┘  │
│                                       │                                        │
│                                       ▼                                        │
│  ┌──────────────────────────────────────────────────────────────────────────┐  │
│  │                              DAEMON CORE                                 │  │
│  │                                                                          │  │
│  │  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  │  │
│  │  │ Agent Runner │  │  Deep Loop   │  │  Sub-Agents  │  │   Memory     │  │  │
│  │  │              │  │              │  │              │  │              │  │  │
│  │  │ prompt →     │  │ think → act  │  │ ephemeral    │  │ last 6 runs  │  │  │
│  │  │ model → out  │  │ → observe    │  │ at runtime   │  │ as context   │  │  │
│  │  └──────────────┘  └──────────────┘  └──────────────┘  └──────────────┘  │  │
│  │                                                                          │  │
│  │  ┌──────────────────────────────────────────────────────────────────┐    │  │
│  │  │                       TOOL EXECUTOR                              │    │  │
│  │  │        TOOL_CALL → shell handler → result → agent                │    │  │
│  │  └──────────────────────────────────────────────────────────────────┘    │  │
│  └──────────────────────────────────────────────────────────────────────────┘  │
│                                       │                                        │
│                                       ▼                                        │
│  ┌──────────────────────────────────────────────────────────────────────────┐  │
│  │                               BACKENDS                                   │  │
│  │                                                                          │  │
│  │  ┌──────────────────────────────────┐  ┌──────────┐  ┌───────────────┐  │  │
│  │  │        Model Backend (pluggable) │  │ Storage  │  │  Shell Tools  │  │  │
│  │  │                                  │  │          │  │               │  │  │
│  │  │  Ollama · DeepSeek · OpenRouter  │  │ SQLite   │  │ ~/.agenta/    │  │  │
│  │  │  OpenAI · any OpenAI-compat API  │  │ Postgres │  │ tools/        │  │  │
│  │  └──────────────────────────────────┘  └──────────┘  └───────────────┘  │  │
│  └──────────────────────────────────────────────────────────────────────────┘  │
└────────────────────────────────────────────────────────────────────────────────┘
```

---

## 📝 Notes

- The daemon must be running for CLI operations that use socket RPC.
- Scheduling, triggers, Telegram, and the REST API all run inside the daemon process.
- `agenta daemon status` is the source of truth for daemon health.
- Sub-agents are ephemeral — not saved to the database, not listable.
- Tools live in `~/.agenta/tools/` — decoupled from the repo, safe across upgrades.
