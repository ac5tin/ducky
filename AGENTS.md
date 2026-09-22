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
- `src-tauri/src/agent.rs` — chat loop: stream → tool calls → approvals → tool results; also subagents (`ducky__subagent` tool: parallel/nested runs via `RunScope`, depth-capped; spawns take an optional `agent` type resolving to a user-editable `SubagentConfig` in Settings — persona, optional model/effort overrides, enforced tool allowlists; defaults General-Purpose + Explore are seeded into config once — see `docs/superpowers/specs/2026-09-20-subagents-design.md` and `2026-09-21-subagent-definitions-design.md`).
- Agent modes (`Default`/`ReadOnly`/`Plan`/`Auto`) are enforced by one predicate,
  `mode_allows` in `agent.rs`: it filters the tool list the model sees *and* gates
  `execute_tool`, so the two cannot disagree. The mode is read from the conversation
  on every loop iteration, which is why a switch applies mid-turn. `ducky__present_plan`
  and `ducky__set_mode` are "control tools": chat flow, main-scope only, and they skip
  the approval gate. MCP tools are allowed in read-only modes only when the server sets
  `readOnlyHint` — the one sanctioned exception to the untrusted-annotation posture, per
  `docs/adr/0004-trust-mcp-readonly-hint-for-readonly-modes.md`.
- `src-tauri/src/mcp/` — `manager.rs` (one rmcp client per server, caches, subscriptions), `handler.rs` (client capabilities: elicitation/sampling/roots), `bridge.rs` (interactive approvals ↔ UI events), `tests.rs`.
- `src-tauri/src/providers/` — `openai.rs` + `anthropic.rs` adapters.
- Also: `config.rs` (AppConfig + secrets), `oauth.rs` (OAuth 2.1 for remote MCP servers), `terminal.rs` (portable-pty), `builtin/` (built-in fs/html/web tools), `catalog.rs`, `title.rs`, `compact.rs` (`/compact` summarisation), `snapshot.rs` (hidden git repos backing `/undo` — see ADR-0002).
- Slash commands (`/compact`, `/undo`, `/init`) are parsed in `src/slashCommands.ts` and routed in `store.ts` `send()` before the busy-queue branch; their design is in `docs/adr/0002-slash-commands-undo-compact.md`.

## Rules & gotchas

- **Secrets never reach the webview.** API keys live in a 0600-permission secrets file managed by `config.rs`; keep it that way.
- **MCP trust posture**: tool annotations are untrusted hints; consent is per-call with allow-list memory; request-state blobs are echoed opaquely, never parsed. Don't weaken this.
- **rmcp SEP-2577 deprecation warnings are intentionally silenced** — see `docs/adr/0001-silence-rmcp-sep-2577-deprecations.md`. Don't migrate off the deprecated client features.
- Compatibility/maintenance decisions are recorded as ADRs in `docs/adr/` — read them before touching those areas, and add one for decisions of that kind.
- Release CI (`.github/workflows/release.yml`) stamps the version from the git tag into `package.json`, `tauri.conf.json`, and `Cargo.toml` — don't hand-bump versions for releases.
