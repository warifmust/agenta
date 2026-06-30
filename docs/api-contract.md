# Agenta HTTP API Contract (v1)

The single boundary between the **daemon** and any frontend (web dashboard, mobile,
third-party). The browser never touches the Unix socket — it speaks HTTP/SSE to this API,
which is just another entry point into the same daemon core the TUI uses over IPC.

```
Browser / Dashboard ──HTTP + SSE──▶  REST API (:8789)  ──IPC──▶  Daemon core ──▶ DB / providers
TUI / CLI           ──Unix socket (DaemonRequest)─────────────▶  Daemon core
```

---

## 1. Design principles (the load-bearing decisions)

These are the parts that are expensive to change later. The framework behind them is not.

1. **Versioned base path** — everything under `/api/v1`. Lets response shapes evolve without
   breaking deployed frontends. Adopt now even as a solo dev.
2. **DTOs are not IPC types** — the API exposes its own request/response shapes. Do **not**
   serialize `DaemonRequest`/`DaemonResponse` (which use untyped `serde_json::Value`) onto the
   wire. The API contract must be typed and stable independently of internal refactors.
3. **Consistent envelopes**
   - Success: the resource or a typed object, directly.
   - Error: always `{ "error": { "code": "snake_case", "message": "human readable" } }`
     with the right HTTP status. Never leak raw `anyhow` strings.
4. **Auth** — keep the existing model: `Authorization: Bearer <token>` or `x-api-key: <token>`,
   enforced only when `api_token` is set. Applies to every route except `/health`.
5. **CORS** — required. The dashboard is a different origin. Add a `tower-http` `CorsLayer`
   restricted to a configurable `dashboard_origin` (default `http://localhost:5173`).
6. **Streaming = SSE** — agent runs stream tokens/progress over Server-Sent Events, not
   WebSocket. The daemon already produces an `mpsc::UnboundedSender<String>` progress channel
   (`run_agent_sync_execution_with_progress`); SSE maps onto it directly and is far simpler than
   WS for a one-directional token stream. Reserve WS only if you later need bidirectional control.
7. **IDs accept name or uuid** — the daemon already resolves either; keep that ergonomic in the API.

---

## 2. Resource surface

### 2.1 System

| Method | Path | Purpose |
|--------|------|---------|
| `GET`  | `/api/v1/health` | Liveness. No auth. `{ "status": "ok" }` |
| `GET`  | `/api/v1/status` | Daemon health: `{ running, pid, version, uptime_seconds }` |
| `GET`  | `/api/v1/dashboard` | Aggregated summary for the landing view (see 3.1) |

### 2.2 Agents

| Method | Path | Purpose |
|--------|------|---------|
| `GET`    | `/api/v1/agents` | List agents. Query: `?include_system=false` |
| `POST`   | `/api/v1/agents` | Create. Body: `CreateAgentRequest`. Returns the created `Agent` |
| `GET`    | `/api/v1/agents/:id` | Get one agent (id or name) |
| `PATCH`  | `/api/v1/agents/:id` | Partial update. Body: `UpdateAgentRequest` (all fields optional) |
| `DELETE` | `/api/v1/agents/:id` | Delete |
| `POST`   | `/api/v1/agents/:id/run` | Start a run. Body: `{ input }`. Returns `{ execution_id }` |
| `POST`   | `/api/v1/agents/:id/stop` | Stop a running agent |
| `GET`    | `/api/v1/agents/:id/stream` | **SSE** — live run progress (see 4) |
| `GET`    | `/api/v1/agents/:id/executions` | Run history. Query: `?limit=20` |
| `GET`    | `/api/v1/agents/:id/logs` | Logs. Query: `?execution_id=&lines=50` |

**Agent ⇄ Tool wiring** (first-class, replaces the CLI fetch-modify-update dance):

| Method | Path | Purpose |
|--------|------|---------|
| `GET`    | `/api/v1/agents/:id/tools` | List tools attached to this agent |
| `POST`   | `/api/v1/agents/:id/tools` | Attach an installed tool. Body: `{ tool_name }` |
| `DELETE` | `/api/v1/agents/:id/tools/:tool_name` | Detach a tool (keeps it installed) |

### 2.3 Tools

| Method | Path | Purpose |
|--------|------|---------|
| `GET`    | `/api/v1/tools` | List all tools (DB-registered ∪ disk-installed, deduped by name) |
| `POST`   | `/api/v1/tools` | Create a tool. Body: `CreateToolRequest` |
| `GET`    | `/api/v1/tools/:id` | Get one (id or name) |
| `PATCH`  | `/api/v1/tools/:id` | Update (name, description, parameters, handler, enabled) |
| `DELETE` | `/api/v1/tools/:id` | Delete |
| `POST`   | `/api/v1/tools/:id/run` | Run manually. Body: `{ input }`. Returns `{ execution_id }` |
| `GET`    | `/api/v1/tools/:id/executions/:eid` | Tool execution result |
| `GET`    | `/api/v1/tools/:id/logs` | Tool logs. Query: `?execution_id=&lines=50` |

**Registry:**

| Method | Path | Purpose |
|--------|------|---------|
| `GET`  | `/api/v1/registry/tools` | List tools available in the configured registry |
| `POST` | `/api/v1/tools/pull` | Pull + install from registry. Body: `{ name, version?, attach? }` |

### 2.4 Scripts

| Method | Path | Purpose |
|--------|------|---------|
| `GET`    | `/api/v1/scripts` | List |
| `POST`   | `/api/v1/scripts` | Create. Body: `CreateScriptRequest` |
| `GET`    | `/api/v1/scripts/:id` | Get one |
| `PATCH`  | `/api/v1/scripts/:id` | Update (name, handler, description, schedule, enabled) |
| `DELETE` | `/api/v1/scripts/:id` | Delete |
| `POST`   | `/api/v1/scripts/:id/run` | Run now. Returns `{ execution_id }` |
| `GET`    | `/api/v1/scripts/:id/logs` | Logs. Query: `?execution_id=&lines=50` |

### 2.5 Config & providers (dashboard support)

| Method | Path | Purpose |
|--------|------|---------|
| `GET`  | `/api/v1/config` | Current config, secrets redacted (for the settings screen) |
| `PATCH`| `/api/v1/config` | Update safe fields (default_provider, default_model, timezone, registry_*) |
| `GET`  | `/api/v1/providers` | Configured providers + which have keys present |
| `GET`  | `/api/v1/providers/:name/models` | Available models (Ollama: live list; cloud: known set) — powers model dropdowns |

---

## 3. Key payloads

### 3.1 `GET /dashboard`
```json
{
  "daemon": { "running": true, "version": "1.1.1", "uptime_seconds": 3600 },
  "counts": { "agents": 6, "tools": 9, "scripts": 3 },
  "recent_executions": [ /* last 10 ExecutionSummary, across all agents */ ],
  "agents": [ { "id": "...", "name": "CORAL", "status": "idle", "last_run": "...", "run_count": 42 } ]
}
```

### 3.2 `CreateAgentRequest`
```json
{
  "name": "CORAL",
  "model": "deepseek-chat",
  "provider": "deepseek",
  "system_prompt": "You are CORAL...",
  "description": "Research companion",
  "temperature": 0.7,
  "max_tokens": 8192,
  "memory_enabled": true,
  "execution_mode": "once",
  "schedule": null
}
```
`UpdateAgentRequest` = same fields, all optional (PATCH semantics).

### 3.3 Error envelope
```json
{ "error": { "code": "agent_not_found", "message": "Agent 'CORAL' not found" } }
```

---

## 4. Streaming contract (SSE)

`GET /api/v1/agents/:id/stream?execution_id=<id>` — `Content-Type: text/event-stream`.
Frontend opens this after `POST /run` returns an `execution_id` (or the run endpoint can
return an SSE stream directly). Event types:

```
event: progress
data: {"iteration":1,"text":"Reading file..."}

event: tool_call
data: {"tool":"read_file","parameters":{"path":"..."}}

event: tool_result
data: {"tool":"read_file","result":"...","truncated":false}

event: complete
data: {"status":"completed","output":"...","iterations":4}

event: error
data: {"code":"provider_timeout","message":"..."}
```

Backend: bridge the existing `mpsc::UnboundedSender<String>` from
`run_agent_sync_execution_with_progress` into an `axum::response::Sse` stream. The channel
plumbing already exists — only the HTTP-side adapter is new.

---

## 5. Gap analysis vs. current `src/daemon/rest_api.rs`

Legend: ✅ exists · 🔧 update · ➕ add (daemon method already exists, just wire a handler) ·
🏗️ build (needs new daemon/server logic)

### Cross-cutting (do first)
- 🔧 Prefix all routes with `/api/v1`.
- 🔧 Replace raw-string errors (`internal_error`) with the structured error envelope.
- 🔧 `create_agent`/`run_agent` currently stuff data into a `message` string — return typed
  objects (`Agent`, `{ execution_id }`) instead.
- 🏗️ Add `CorsLayer` (`tower-http`) keyed off a new `dashboard_origin` config field.
- 🔧 Switch agent update from `PUT` (full object) to `PATCH` (partial) so the dashboard can
  send only changed fields.

### Agents
| Endpoint | State |
|----------|-------|
| list / get / create / update / delete | ✅ (update shapes per above) |
| `POST /run` | ✅ (return `{execution_id}` not message) |
| `POST /stop` | ➕ `daemon.stop_agent` exists, unexposed |
| `GET /stream` (SSE) | 🏗️ progress channel exists; SSE adapter is new |
| `GET /executions`, `GET /logs` | ✅ |
| `GET/POST/DELETE /agents/:id/tools` | 🏗️ logic lives in CLI `commands.rs`; promote to daemon methods |

### Tools — **entire resource missing from REST**
| Endpoint | State |
|----------|-------|
| list / get / create / update / delete | ➕ all `daemon.*_tool` methods exist, zero REST handlers |
| `POST /tools/:id/run`, executions, logs | ➕ `daemon.run_tool` / `get_tool_execution` / `get_tool_logs` exist |
| `GET /registry/tools` | 🏗️ new — list from GitHub registry |
| `POST /tools/pull` | 🏗️ pull logic exists in CLI only; move into a daemon method so REST + CLI + TUI share it |

### Scripts — **entire resource missing from REST**
| Endpoint | State |
|----------|-------|
| full CRUD + run + logs | ➕ all `daemon.*_script` methods exist, zero REST handlers |

### System / config
| Endpoint | State |
|----------|-------|
| `GET /health` | ✅ (move under `/v1`) |
| `GET /status` | ➕ daemon `Ping`→`Status` exists (running/pid/version); add uptime |
| `GET /dashboard` | 🏗️ new aggregation over existing list methods |
| `GET/PATCH /config` | 🏗️ new — read/write `config.toml`, redact secrets |
| `GET /providers`, `/providers/:name/models` | 🏗️ new — Ollama has a live model list; cloud providers static |

### Chat (biggest new build)
- 🏗️ There is **no conversational endpoint** today — chat exists only via Telegram polling and
  the TUI's local streaming loop. For a dashboard chat panel you need either:
  - reuse `POST /agents/:id/run` + `GET /stream` per message (simplest — no persistent thread), or
  - a new **thread/conversation** concept (`POST /agents/:id/chat`, `GET /agents/:id/threads`)
    if you want persisted multi-turn history in the DB. Decide this explicitly; it's the one
    place the data model genuinely grows.

---

## 6. Build order (suggested)

1. **Cross-cutting**: `/v1`, error envelope, CORS, typed responses. (Unblocks everything.)
2. **Tools + Scripts REST** — pure wiring of existing daemon methods. Fast, high value.
3. **`/status` + `/dashboard`** — enough to render a read-only dashboard end-to-end.
4. **SSE `/stream`** — makes runs feel live.
5. **Agent⇄tool endpoints + `/tools/pull`** — promote CLI logic into daemon methods.
6. **Config/providers** — settings screen.
7. **Chat thread model** — only if you want persisted conversations.

Steps 1–3 alone give a working read + manage dashboard against the real daemon.
