<p align="center">
  <img src="assets/agenta-logo.svg" width="220" alt="Agenta Logo">
</p>

<h1 align="center">Agenta</h1>

<p align="center">
  <strong>Local-first AI agent platform.</strong> Build, run, schedule, and operate autonomous agents from one CLI + daemon with triggers, tools, REST API, and Telegram integration. Built with <strong>Rust</strong>.
</p>

---

## What You Get

- Local agent management (`create`, `update`, `run`, `logs`, `list`)
- Daemon runtime with scheduling and triggers
- Optional Postgres backend (SQLite by default)
- Optional REST API + Swagger
- Optional Telegram/WhatsApp webhook gateway

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

# Chat gateway
chat_gateway_port = 8790
telegram_bot_token = "<telegram-bot-token>"
telegram_default_agent = "travel-guide"
whatsapp_default_agent = "travel-guide"

# REST API
api_port = 8789
api_token = "replace-with-a-strong-token"
```

Notes:
- If `database_url` is set, Agenta uses Postgres.
- If `database_url` is not set, Agenta uses SQLite at `database_path`.
- `telegram_*` / `whatsapp_*` are optional and only needed for chat gateway integrations.
- `api_token` is optional; if set, API endpoints require auth.

### 5. Start Daemon

```bash
agenta daemon start
agenta daemon status
```

### 6. Create Your First Agent

```bash
agenta create \
  --name "travel-guide" \
  --model "qwen3:latest" \
  --prompt "You are a practical travel assistant. Plain text only."
```

### 7. Run It

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
agenta view        # View runtime data (e.g., executions)
agenta tool        # Tool lifecycle (create/get/list/update/delete/run/logs)
agenta daemon      # start/stop/status/restart daemon
```

---

## Common Workflows

### Update Prompt / Model

```bash
agenta update travel-guide --prompt "New system prompt"
agenta update travel-guide --model "qwen3:latest"
```

### Schedule Daily Run (10:00)

```bash
agenta update travel-guide --mode scheduled --schedule "0 10 * * *"
```

### Switch Back to Manual Only

```bash
agenta update travel-guide --mode once --schedule ""
```

### Attach Tools

```bash
agenta update travel-guide --tools tools/echo.json,tools/another.yaml
```

### Manage First-Class Tools

Create tool:

```bash
agenta tool create \
  --name web-fetch \
  --description "Fetch web content via custom script" \
  --parameters '{"type":"object","properties":{"url":{"type":"string"}},"required":["url"]}' \
  --handler "/Users/you/bin/web_fetch_tool"
```

If you omit `--handler`, `agenta` auto-creates `./tools/<name>.sh` and uses it as the handler.

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

## Database Configuration

### SQLite (Default)

No extra setup needed; Agenta uses local `database_path`.

### Postgres (Optional)

```toml
database_url = "postgres://postgres:<password>@localhost:5432/postgres"
```

If `database_url` is set, daemon uses Postgres. If not set, daemon uses SQLite.

---

## Telegram / WhatsApp Integration

Add to config:

```toml
chat_gateway_port = 8790
telegram_bot_token = "<telegram-bot-token>"
telegram_default_agent = "travel-guide"
whatsapp_default_agent = "travel-guide"
```

Webhook endpoints:

- Telegram: `POST http://<host>:8790/telegram/webhook`
- WhatsApp (Twilio): `POST http://<host>:8790/whatsapp/webhook`

Routing behavior:

- Default agent: `telegram_default_agent` / `whatsapp_default_agent`
- Inline override: `/agent <agent-name> <message>`

Important for Telegram:

- Must be public HTTPS URL (localhost is not reachable by Telegram)
- For local dev, use tunnel (ngrok/cloudflared)

Example tunnel:

```bash
ngrok http http://127.0.0.1:8790
```

Then set webhook to:

`https://<ngrok-domain>/telegram/webhook`

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

### Telegram ngrok `8012` / `connection refused`

Daemon or chat gateway not reachable on `8790`.

- Check daemon is running
- Check config has `chat_gateway_port = 8790`
- Start tunnel to `127.0.0.1:8790`

### Swagger resolver errors / stale docs

Hard refresh browser or reopen Swagger URL after daemon restart.

---

## Notes

- Daemon must be running for CLI operations that use socket RPC.
- Scheduling, triggers, chat gateway, and REST API all run inside daemon process.
- Use `agenta daemon status` as source of truth for daemon health.
