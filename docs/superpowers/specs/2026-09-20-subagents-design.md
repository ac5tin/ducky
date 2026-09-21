# Subagents — Design

Date: 2026-09-20
Status: implemented (this document records the design as built)

## Goal

The main agent can spawn subagents: autonomous runs with a fresh, isolated
context that complete one self-contained task and return a final answer.
Multiple subagents spawned in the same assistant message run in parallel;
subagents can spawn their own subagents, up to a fixed depth. Subagents always
inherit the conversation's provider, model and reasoning effort — by
construction, not by parameter.

## Decisions

- **One tool call = one subagent.** `ducky__subagent` takes a single required
  `task` string. Parallelism comes from the model issuing several calls in one
  assistant message, which the loop executes concurrently (up to
  `MAX_CONCURRENT_SUBAGENTS = 4`; extras in the same message run sequentially
  in call order).
- **Strict inheritance.** No model/effort override parameters. `run_subagent`
  reads `provider_id`/`model` from the conversation meta at spawn time; effort
  is read per provider round from the same meta, exactly like the main loop —
  so a subagent always tracks the conversation's settings.
- **Context isolation.** A subagent starts from `[System(preamble + cwd
  grounding), User(task)]` only. It never sees the parent conversation and
  cannot ask it questions; the preamble tells it to act autonomously and end
  with a complete final answer.
- **Ephemeral transcripts.** Subagent histories are never persisted. Only the
  final assistant text flows back to the parent as the `Msg::ToolResult`.
  `/undo`, compaction and the conversation file are untouched.
- **Depth cap 3** (`MAX_SUBAGENT_DEPTH`): a subagent whose own depth is 3 gets
  an error result ("handle this task yourself") instead of spawning.
- **Trust posture unchanged.** The tool is registered `read_only: false`, so
  spawning is consent-gated like any mutating tool ("Always allow" works via
  the normal `builtin/ducky__subagent` tool rule). Every tool call a subagent
  makes goes through the same per-call approval pipeline with the same
  allow-list memory — delegation grants nothing.

## Backend shape

- `agent.rs` holds a `RunScope` (`Main` | `Subagent { tool_call_id, depth }`).
  The former `loop_turn` became `agent_loop(..., scope)`:
  - Main streams `ChatDelta` and persists after every message, exactly as
    before. Subagent scope streams `SubagentDelta` (keyed to the parent's tool
    call), skips reasoning deltas, still forwards `Usage`, never persists, and
    returns the final assistant text.
  - `tool_call_update` events for a subagent's internal tool calls carry
    `parent_tool_call_id`; approval requests still surface in the normal
    global modal queue (attribution is per conversation, which subagents
    share).
- Tool execution: a pre-pass gates (approval) and spawns subagent calls as
  tokio tasks; the ordered pass runs every other tool sequentially and awaits
  subagent handles, appending all results in call order. Every subagent run
  crosses a task boundary — that is also what keeps `agent_loop`'s future
  sized and `Send` despite the recursion subagent → loop → subagent.
- Cancellation: subagents run on `ct.child_token()`, so Stop cancels the whole
  tree; `cancel_for_conversation` needed no change.
- Bridge: `conversation_ctx` became a refcounted slot (`acquire_conversation_ctx`
  → guard). Nested runs of the same conversation share it; the last guard out
  clears the slot and the sampling backend. Previously a finishing nested run
  would have cleared the parent's attribution.

## Frontend shape

- `subagent_delta` events are batched in the same per-animation-frame buffer
  as chat deltas and appended to the matching `ToolItem`'s `subagent_text`
  (pure helper in `src/subagents.ts`).
- `tool_call_update` events with `parent_tool_call_id` become a one-line
  activity label (`ducky__fs_read · running`) on the subagent card instead of
  a card of their own.
- **Card identity (2026-09-21):** the spawn schema has optional `name` (short
  label, e.g. "repo-explorer") and `description` (one-line brief) params;
  the card header shows `{name} — {brief}` with fallbacks to "Subagent" and
  the task's first line. The `running` card event carries `subagent`
  (`SubagentMeta`: provider_id, model, effort — read from the conversation
  meta at spawn, model falling back to the provider default), rendered as
  chips above the transcript. The body shows the meta chips, live transcript
  (markdown, streaming caret), an activity line, the full task as readable
  text (replacing the raw Arguments JSON for subagent cards), and the final
  answer as the result section. Name/brief survive reload via persisted
  args; transcript and meta are session state — same behavior as reasoning
  today.
- **Null-patch fix (2026-09-21):** `stripEvent` previously mapped absent
  event fields to `null` own-properties, so a partial patch (the bare
  `running` update) wiped the `tool`/`args` set by `pending_approval` —
  cards showed the "tool" fallback and `Arguments: null` (pre-existing for
  all tool cards, exposed by subagents). It now keeps only fields the event
  actually carries, and a fresh `running` attempt clears stale
  result/content fields alongside `task`.

## Testing

- `agent_tests.rs`: a scripted `MockProvider` (injected via a `#[cfg(test)]`
  hook in `provider_for`, keyed by the last user message = the task) drives
  `run_turn` end-to-end: single spawn, two spawns proven concurrent with a
  barrier (sequential execution would deadlock past the timeout), nesting
  with parent-attributed internal cards, the depth cap, and cancel
  propagation.
- `bridge.rs`: ctx refcount guard unit test.
- `src/subagents.test.mjs`: pure helper tests (delta folding, task snippet,
  activity label).
