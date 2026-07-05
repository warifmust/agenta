<table>
  <tr>
    <td width="30%" align="center" valign="middle">
      <img src="assets/agenta-logo-2.png" alt="Agenta Logo" width="100%">
    </td>
    <td width="70%" valign="middle">
      <h1>Agenta</h1>
      <p><strong>Define it. Deploy it. Forget it.</strong></p>
      <p>
        A <strong>thin</strong>, self-hosted runtime for building, running, and observing AI
        agents, defined with config, not code. One Rust binary: no Python, no framework glue,
        no cloud account. Deploy agentic systems (and RAG over your own documents) straight
        from your terminal, on your machine, with your models. Made by <strong>Arifmustaffa Research</strong>.
      </p>
      <p><em>Configure agents don't program them. Run them like infrastructure, from the terminal.</em></p>
    </td>
  </tr>
  
</table>

<p align="center">
  <a href="https://agenta.arifmustaffa.com"><strong>Docs</strong></a> ·
  <a href="https://agenta.arifmustaffa.com/docs/quickstart">Quickstart</a> ·
  <a href="https://agenta.arifmustaffa.com/docs/roadmap">Roadmap</a> ·
  <a href="#-install">Install</a>
</p>

---

## 🚀 Install

```bash
curl -fsSL https://raw.githubusercontent.com/warifmust/agenta/main/install.sh | bash
```

Installs the binary and launches the setup wizard. Then open the dashboard with `agenta`.

Full walkthrough → **[Quickstart](https://agenta.arifmustaffa.com/docs/quickstart)**.

---

## 💡 Why Agenta

- 🧩 **No-code agents** — define agents through config and the CLI, not application code. No LangChain or Strands, no Python or JS glue. (Custom tools are the one place code appears and <strong>MIND</strong> scaffolds those for you.)
- ⌨️ **Terminal-native** — create, run, schedule, and inspect agents from the CLI, a TUI dashboard, or an interactive shell. Built for people who live in the terminal.
- 🏠 **Self-hosted & local** — your machine, your models via Ollama, your data. Cloud providers are optional, never required. No subscription to run.
- 📚 **RAG built in** — ingest your own documents and attach a knowledge base to any agent, with grounded, cited answers. Not a bolt-on.
- 🔍 **Know your agents** — memory, logs, tool-call traces, Telegram chat, and a dashboard let you observe and direct a standing team of agents over time — not just fire one-off tasks.
- 🌱 **A gentle on-ramp to agentic AI** — new to this? Explore agents hands-on without first mastering LangChain, Strands, or a pile of Python. Learn by running, on your own pace.

---

## ✨ What Agenta Can Do

| | |
|---|---|
| 🤖 **Agents without code** | Define an agent with a model, prompt, and config no LangChain, no Python. Deploy from a single CLI command. |
| ⏰ **Runs itself** | Cron schedules, file-watchers, and webhooks agents that fire on their own, not just when you ask. |
| 🛠️ **Tools built by MIND** | Describe a tool in plain language; MIND generates the manifest and handler for you. |
| 📚 **RAG over your docs** | Ingest PDF/TXT (OCR for image-based text), attach a knowledge base to any agent, get answers cited to source + page. *(Images & audio incoming.)* |
| 🪄 **A team, not a task** | Agents spawn ephemeral sub-agents and delegate to named agents, coordinated by <strong>MIND</strong>. |
| 💬 **Reachable anywhere** | Telegram bots, a TUI dashboard, and an interactive shell talk to your agents where you already are. |
| 🔌 **Any model, per agent** | Ollama local by default; swap to DeepSeek, OpenRouter, or OpenAI per agent no re-architecture. |
| 📊 **Nothing hidden** | Execution logs, tool-call traces, and full run history for every agent. |
| 🧠 **Deep by default** | Every agent runs harnessed: multi-step reasoning, iterative tool use, built-in file tools, and memory — no flags. |
| 📦 **Yours to move** | Export/import agents as JSON/YAML with auto-backup, plus a REST API + Swagger when you want to integrate. |

---

## 📖 Documentation

Everything — setup, configuration, commands, and guides — lives at **[agenta.arifmustaffa.com](https://agenta.arifmustaffa.com)**.

| | |
|---|---|
| **[Getting Started](https://agenta.arifmustaffa.com/docs/intro)** | Install, first-time setup, first agent |
| **[Core Concepts](https://agenta.arifmustaffa.com/docs/concepts/agent-lifecycle)** | How agents, execution, tools, and memory work |
| **[Guides](https://agenta.arifmustaffa.com/docs/guides/agents/create-an-agent)** | Create agents, add tools, schedule, multi-agent, self-host |
| **[CLI Reference](https://agenta.arifmustaffa.com/docs/reference/cli)** | Every command and flag |
| **[Roadmap](https://agenta.arifmustaffa.com/docs/roadmap)** | Where Agenta is headed |

---

## 📄 License

MIT © 2026 Arif Mustaffa
