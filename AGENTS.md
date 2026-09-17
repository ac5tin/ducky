# AGENTS.md

Ducky is a cross-platform MCP client desktop app built with Tauri 2: a Rust backend (`src-tauri/`) that talks to LLM providers (OpenAI-compatible + Anthropic, SSE streaming) and MCP servers (via the `rmcp` SDK), and a React 19 + TypeScript frontend (`src/`).

## Commands

Uses **npm**, not pnpm, despite the README saying pnpm — `package-lock.json` is committed and CI runs `npm ci`.

```bash
npm install
npm run tauri dev        # run the app (vite on fixed port 1420)
npm run build            # tsc (typecheck) + vite build
node --test src/*.test.mjs   # frontend tests (node:test, needs Node ≥23 for .ts type stripping)

cd src-tauri
cargo check              # fast typecheck
cargo test               # rust unit + MCP tests (mcp/tests.rs has rmcp-server-backed tests)
cargo clippy
```

Rust 1.85+ required (`rust-version` in Cargo.toml). Linux builds need `webkit2gtk-4.1`.

## Layout

- `src/` — React + TS (strict, `noUnusedLocals`/`noUnusedParameters`), Tailwind v4 (CSS-first, no config file), zustand (`store.ts`), no router; components under `chat/`, `connectors/`, `modals/`, `settings/`. No import aliases — use relative paths.
- `src-tauri/src/commands.rs` — the ~40-command Tauri command surface; frontend calls backend only through these.
- `src-tauri/src/events.rs` — `BackendEvent` stream to the webview on `backend://event`.
- `src-tauri/src/agent.rs` — chat loop: stream → tool calls → approvals → tool results.
- `src-tauri/src/mcp/` — `manager.rs` (one rmcp client per server, caches, subscriptions), `handler.rs` (client capabilities: elicitation/sampling/roots), `bridge.rs` (interactive approvals ↔ UI events), `tests.rs`.
- `src-tauri/src/providers/` — `openai.rs` + `anthropic.rs` adapters.
- Also: `config.rs` (AppConfig + secrets), `oauth.rs` (OAuth 2.1 for remote MCP servers), `terminal.rs` (portable-pty), `builtin/` (built-in fs/html/web tools), `catalog.rs`, `title.rs`.

## Rules & gotchas

- **Secrets never reach the webview.** API keys live in a 0600-permission secrets file managed by `config.rs`; keep it that way.
- **MCP trust posture**: tool annotations are untrusted hints; consent is per-call with allow-list memory; request-state blobs are echoed opaquely, never parsed. Don't weaken this.
- **rmcp SEP-2577 deprecation warnings are intentionally silenced** — see `docs/adr/0001-silence-rmcp-sep-2577-deprecations.md`. Don't migrate off the deprecated client features.
- Compatibility/maintenance decisions are recorded as ADRs in `docs/adr/` — read them before touching those areas, and add one for decisions of that kind.
- Release CI (`.github/workflows/release.yml`) stamps the version from the git tag into `package.json`, `tauri.conf.json`, and `Cargo.toml` — don't hand-bump versions for releases.
