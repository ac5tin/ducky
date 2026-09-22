# Ducky 🦆

**A friendly, cross-platform MCP client.** Ducky connects your favourite AI models — OpenAI, Claude, Z.ai, OpenCode Zen, Ollama, and any OpenAI- or Anthropic-compatible endpoint — to [MCP (Model Context Protocol)](https://modelcontextprotocol.io) servers, so your assistant can actually *do* things: read files, search the web, query databases, and more.

Built with **Rust + Tauri 2** for a small footprint and native performance.

```
┌────────────────────────────────────────────────────────────┐
│  Ducky                                                     │
│  ┌──────────┐  ┌────────────────────────────────────────┐  │
│  │ Chats    │  │  You: What's the weather in Tokyo?     │  │
│  │ Connect..│  │  AI: Let me check. [weather__get_weather]│
│  │ Settings │  │      ✅ tool approved · ran · 72°F     │  │
│  │          │  │  AI: It's 22°C and partly cloudy 🌤     │  │
│  └──────────┘  └────────────────────────────────────────┘  │
└────────────────────────────────────────────────────────────┘
```

## Why Ducky

- **Zero-config start** — a short wizard picks your AI provider, tests the key, and offers one-click connectors. No JSON files required (but you can paste configs from Claude Desktop, Cursor, etc. — Ducky imports them).
- **Full MCP 2026-07-28 support** — the newest protocol revision, implemented on the official [`rmcp`](https://github.com/modelcontextprotocol/rust-sdk) SDK (Tier 1 conformance).
- **Architecture decisions** — compatibility and maintenance decisions are recorded in [`docs/adr`](docs/adr/).
- **Human in the loop** — every tool call asks before it runs (configurable), with plain-language approval cards.
- **Agent modes** — Default, Read-only, Plan and Auto. Read-only and Plan block every
  tool that is not marked read-only, including inside subagents; Plan makes the agent
  research and present a plan you approve before it changes anything, and approval
  returns the chat to Default. Auto lets the agent pick the mode itself. Switch from the
  chat toolbar or with `Shift+Tab`.
- **Multi-provider** — switch models mid-conversation. Local models (Ollama, LM Studio) work with no API key.
- **Reasoning effort levels** — pick an effort per conversation straight from the model picker. Supported levels come from [models.dev](https://models.dev) (e.g. `low/high/max` for GLM‑5.3, `none…xhigh` for GPT‑5.x) and are mapped to each provider's native API: `reasoning_effort` (+ `thinking`) for OpenAI-compatible endpoints, extended-thinking budgets for Claude.
- **Private by design** — API keys are stored in a `0600`-permission secrets file, never sent to the webview.

## MCP 2026-07-28 feature coverage

| Area | Support |
| --- | --- |
| Transports | stdio (child process) + Streamable HTTP |
| Protocol eras | Modern stateless (`server/discover`, per-request `_meta` capabilities/protocol version) **and** legacy `initialize` handshake, with automatic era detection (Auto lifecycle) |
| Version negotiation | `2026-07-28` preferred; falls back through `2025-11-25`, `2025-06-18`, `2025-03-26`, `2024-11-05`; honours `UnsupportedProtocolVersionError` (-32022) |
| Tools | `tools/list` (pagination, TTL caching), `tools/call` with structured content (`structuredContent`/`outputSchema`), annotations, icons, progress tokens, `isError` execution errors |
| MRTR | Multi-round-trip requests: `InputRequiredResult` → user input → retry with `inputResponses` + `requestState` (drives elicitation/sampling/roots inside tool calls) |
| Client features | Elicitation **form mode** (full schema-driven forms incl. enums, multi-select, formats, defaults) and **URL mode** (consent + browser handoff), Sampling (deprecated, still negotiated), Roots (deprecated, configurable folders) |
| Subscriptions | `subscriptions/listen` streams for tools/prompts/resources list changes + per-resource updates, with graceful fallback to legacy notifications on old servers |
| Resources | list / templates / read / subscribe, text + binary |
| Prompts | list / get with arguments |
| Completions | `completion/complete` for prompt & resource arguments |
| Auth | OAuth 2.1: Protected Resource Metadata discovery (RFC 9728), AS metadata (RFC 8414/OIDC), Dynamic Client Registration (RFC 7591) or pre-registered client ids, PKCE S256, loopback redirect, `resource` indicator (RFC 8707), `iss` validation (RFC 9207), refresh tokens; static bearer tokens also supported |
| Errors | Full JSON-RPC error surfacing incl. reserved `-32020`..`-32022` codes, legacy `-32002` acceptance for resources |
| Extensions | Capabilities negotiated per request; server extensions surfaced in the connector inspector |

## Supported providers

| Preset | Endpoint | Notes |
| --- | --- | --- |
| OpenAI | `api.openai.com/v1` | |
| Claude (Anthropic) | `api.anthropic.com/v1` | native Messages API |
| Z.ai Coding Plan | `api.z.ai/api/coding/paas/v4` | GLM models |
| Z.ai | `api.z.ai/api/paas/v4` | GLM models |
| OpenCode Zen | `opencode.ai/zen/v1` | |
| Ollama | `localhost:11434/v1` | local, no key |
| LM Studio | `localhost:1234/v1` | local, no key |
| OpenRouter / Groq | | one key, many models |
| Custom | any base URL | OpenAI- or Anthropic-compatible |

Model lists are fetched live from each provider (`GET /models`), so you always pick from what your account actually offers.

## Building from source

Prerequisites: [Rust](https://rustup.rs) 1.85+, Node 20+/pnpm, and the [Tauri 2 system dependencies](https://tauri.app/start/prerequisites/) (on Linux: `webkit2gtk-4.1`).

```bash
pnpm install
pnpm tauri dev      # run in development
pnpm tauri build    # produce installers for your OS
```

## Architecture

```
src/                     React + TypeScript UI (Tailwind v4, zustand)
src-tauri/
  src/
    config.rs            AppConfig + secrets (0600) + JSON import + persistence
    providers/           Unified LLM layer: OpenAI-compatible + Anthropic adapters (SSE streaming)
    mcp/
      manager.rs         One rmcp client per server; tools/resources/prompts caches; subscriptions
      handler.rs         Client capabilities: elicitation (form+URL), sampling, roots
      bridge.rs          Interactive bridge: approvals/elicitation/sampling ↔ UI events + one-shots
    agent.rs             Chat loop: stream → tool calls → approvals → MRTR → results
    oauth.rs             OAuth 2.1 client flow for remote servers
    commands.rs          Tauri command surface (~40 commands)
    events.rs            BackendEvent stream → webview (`backend://event`)
```

**Security posture** (following the MCP trust & safety guidance): tool annotations are treated as untrusted hints; consent is user-controlled per call with allow-list memory; elicitation URL mode requires explicit consent and shows the full target; OAuth tokens never reach the model context; request-state blobs are echoed opaquely, never parsed.

## Status

Early release. Tested on Linux; macOS/Windows builds use the same Tauri toolchain.

## License

MIT
