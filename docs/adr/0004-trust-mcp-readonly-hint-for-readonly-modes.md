# ADR-0004: Trust the MCP `readOnlyHint` annotation inside read-only agent modes

- **Status:** Accepted
- **Date:** 2026-09-22

## Context

Agent modes (see `docs/superpowers/specs/2026-09-22-agent-modes-design.md`)
add a **ReadOnly** mode and a **Plan** mode. Both promise that the agent cannot
change the workspace or remote state. Every tool call must therefore be
classified as read-only or not before it runs.

Built-in tools already carry a static `read_only` flag
(`src-tauri/src/builtin/mod.rs`). MCP tools do not: the only signal is the
server's `readOnlyHint` annotation, which `src-tauri/src/mcp/manager.rs`
already stores on `ToolEntry.read_only_hint`.

The project's stated trust posture is that "tool annotations are untrusted
hints" (`AGENTS.md`, `README.md`), because a server can send anything.
`ApprovalMode::AutoApproveReadOnly` already acts on the hint, but only to skip
a prompt: a wrong hint there costs a confirmation the user never saw, and the
call still appears in the transcript.

A mode is a different promise. A false `readOnlyHint` during ReadOnly or Plan
means a write happens while the UI says the agent cannot write. Two
alternatives were considered:

1. **Block every MCP tool in ReadOnly and Plan.** Safest, and it keeps the
   posture intact, but it makes plan mode useless for the servers where
   research happens (docs servers, search servers, read-only filesystem
   servers) — exactly the mode where research is the whole point.
2. **Require the user to declare read-only-safe tools** in Settings (a list
   matching `server`, `server/*` or `server/tool`, reusing the existing
   allowlist matcher). Keeps the posture intact, but adds a setting to
   discover and maintain before plan mode is usable at all.

## Decision

In ReadOnly and Plan mode, an MCP tool is allowed only when its
`readOnlyHint` is exactly `true`. An absent hint means "not read-only" (the
MCP specification default), so an unannotated server is blocked.

The hint is used for this one purpose and nowhere else:

- The approval flow is unchanged. `read_only_hint` still only short-circuits a
  prompt under `AutoApproveReadOnly`; it never grants consent.
- The mode gate runs before `tool_rules` and before `gate_approval`, so a
  `ToolRule::Allow` entry cannot unlock a tool in a read-only mode.
- Write-capable tools stay blocked in both modes regardless of annotations,
  and the plan review gate is unaffected.

Because this is a real weakening of the posture, the UI copy must not claim a
guarantee. The mode descriptions say that read-only tools are allowed, not
that nothing can change, and Settings carries a one-line note that a server's
read-only annotation is trusted as-is.

## Consequences

- Plan mode can research with any MCP server that marks its read tools
  correctly, with no setup.
- A server that marks a writing tool as read-only, or that writes while
  claiming read-only, can change state while ReadOnly or Plan is active.
  Ducky cannot detect this; the tool result and the transcript are the only
  record.
- A server that sends no annotations loses access in ReadOnly and Plan. This
  is the safe default and it is visible: the model reports the blocked tool
  and the user can leave the mode.
- The rule is one predicate (`mode_allows`), used both to filter the tool list
  and to gate execution, so a forced call cannot bypass it.

## Revisit when

- MCP ships a verifiable capability manifest, or servers can be pinned to a
  reviewed tool list.
- A server is found lying about `readOnlyHint`, or a user reports a write
  during a read-only mode.
- The user-declared read-only list (alternative 2 above) becomes worth the
  setup cost — it can be added on top of this decision without removing it.
