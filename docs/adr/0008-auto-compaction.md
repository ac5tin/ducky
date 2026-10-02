# ADR-0008: Auto-compaction at a context-window threshold

- **Status:** Accepted
- **Date:** 2026-10-01

## Context

ADR-0002 shipped `/compact` as a manual slash command: one provider call
summarises the whole history with a structured prompt, and the conversation is
replaced by a single `[Conversation compacted]` user message. It listed
"auto-compaction at a token threshold (persisted usage)" as the condition for
revisiting that decision. Two problems remained:

1. Nothing compacted on its own. A user who never ran `/compact` hit the
   provider's context limit and got an error, with no way forward except
   working out that `/compact` exists.
2. The summary was weak. The summariser saw a flattened transcript in which
   every tool result was cut to 1000 characters and every tool argument to 400,
   and the model was asked to write *all* sections — including the exact file
   paths — from that flattened text. Paths and identifiers could be paraphrased
   away, and the newest exchange was destroyed with everything else.

[pi-blackhole](https://github.com/k0valik/pi-blackhole) solves both. Its
compaction (`src/core/summarize.ts`, "VCC") is a deterministic pipeline —
normalize → filter noise → extract goals/files/commits/preferences/unresolved
errors → build a compressed "brief transcript" — with a window-scaled trigger
curve and a cut that keeps the newest user turn.

## Decision

### One algorithm, two triggers

`src-tauri/src/compact.rs` owns a single summarisation path. `/compact` and the
automatic trigger differ only in *when* they run; both produce the same kind of
summary and both keep the newest user turn.

### Trigger: before every model call

`agent_loop` checks the window at the top of each iteration, before the history
is sent, main scope only (subagents never auto-compact). This covers both a new
user message on a full chat and a long tool loop inside one turn — the case that
actually overflows a window. The check runs before the model call on every
round. The loop keeps trying while each pass makes room, and stops for the turn
once a pass leaves the context at or above the trigger (`over_threshold`): the
kept tail is then too big and another pass would land at the same cut. A failed
summary stops the turn's attempts too, so a broken provider is not retried on
every round.

The window comes from the models.dev catalog (`kind` + model). When the model is
absent from the catalog the window is unknown and the trigger stays off — Ducky
never guesses a window size.

Usage is the provider's own `usage.input` for the conversation's last call,
kept in memory on `Agent`, and never less than a characters/4 estimate of the
current history. The estimate covers a fresh app start, where no usage has been
reported yet, and it also covers messages added since that call.

### Cut: a bounded tail, starting where the provider expects

Compaction summarises everything before the cut and keeps the rest as real
messages. The cut is chosen so the kept tail stays bounded:

1. Find the earliest index whose whole tail is within `TAIL_BUDGET_RATIO`
   (25%) of the context window.
2. Cut at the smallest user or assistant message at or after that index, so
   whole turns survive while they fit.
3. If nothing fits, cut at the newest user message — there is no window to
   bound against, so the request the user is waiting on is kept.

The tail can never start on a tool result (the provider would see it as
orphaned), and never after the newest user message unless that whole turn is
bigger than the budget. Manual `/compact` behaves the same way. So the
transcript after either path is normally:

```text
[Conversation compacted]  <- summary carrier
user: <the newest request>
assistant: <its answer>
```

with one exception: inside a long tool loop, a turn that outgrows the tail
budget is cut mid-turn, and the older rounds of that turn are summarised with
the rest.

### Summary: deterministic blocks the model cannot touch

The model writes only the four semantic sections — `## Objective`,
`## Decisions and Constraints`, `## Work State`, `## Next Move`. Everything
factual is produced by code and appended verbatim:

- `## Files And Changes` — `modified:` / `read:` paths from tool calls
- `## User Preferences` — sentences matching preference markers in user text
- `## Outstanding Context` — tool errors no later success on the same tool and
  argument resolved
- `## Transcript` — a compressed brief: thinking dropped, user text ≤256
  tokens, assistant ≤200, tool calls as `name "argument"` one-liners,
  consecutive identical calls collapsed to `xN`, only the last 120 lines

A second compaction merges the previous summary's deterministic blocks forward,
so older files and preferences survive repeated compaction. The previous
summary is still passed to the summariser as `<prior-summary>`.

Tool effects are classified from the last `__` segment of the tool name
(`write`/`edit`/`create`/`mkdir`/… versus `read`/`get`/`list`/`search`/…), so
MCP servers count and not just `ducky__fs_*`.

`## Commits` from pi-blackhole is **not** ported: Ducky exposes no shell tool,
so no deterministic commit source exists.

### Failure is never fatal

A failed summary (provider error, empty output) logs a warning and lets the
turn continue with the history unchanged. There is no retry until the next user
turn; the existing `/compact` refusal to run mid-turn is unchanged.

### Settings

`AppSettings.auto_compact` (default **true**) and
`AppSettings.auto_compact_threshold` (default **80**, clamped 1–99 by
`auto_compact_ratio()`). Old config files load with auto-compaction **on**:
the field uses `#[serde(default = "default_true")]`, because a bare
`#[serde(default)]` would silently load them as off.

A new `auto_compacted` backend event reloads the transcript and tells the user,
because the webview holds a history the backend has just replaced.

### Deliberately not ported from pi-blackhole

Observational memory (background observer/reflector agents, its ledger and
dropper), the append-only immutable segment chain, `#N` global indices and
recall drill-down, tool-output omission markers, the Pi hook API
(`session_before_compact`, `context`, `agent_start/agent_end/turn_end`), the
inline `AgentSession.prototype` monkey-patch, and Pi's `prepareCompaction` /
`keepRecentTokens`. Each depends on Pi's session model, its extension API, or a
recall system Ducky does not have.

## Consequences and risk

- **The brief transcript is a real cost.** It adds roughly 1–2k tokens to every
  later request in the conversation. It buys the detail the model would
  otherwise paraphrase away.
- **Successful tool output is dropped from the summary.** Only errors survive in
  the brief, matching pi-blackhole. The files section names what was touched, so
  the agent can re-read a file, but it cannot see the old contents.
- **The newest request can be summarised.** When a single turn grows past the
  tail budget, the cut lands mid-turn and the user's request survives only in
  the summary. That is the price of bounding the tail; the alternative is a
  request that overflows the window and fails outright.
- **The 25% tail budget is a judgement call.** Too small and context that a
  later turn needed is summarised away; too large and a compacted turn can
  reach the trigger again almost immediately.
- **Usage is not persisted.** After a restart the first check uses the
  characters/4 estimate, which undercounts the system prompt and tool
  definitions. The trigger fires a little late, never early.
- **Path classification is a name heuristic.** A tool named `get_or_create_thing`
  classifies as a write (write verbs are checked first); a read-only tool with an
  unusual verb classifies as ignored and its path never appears.
- **`## User Preferences` is regex-shaped.** Only sentences containing
  `prefer`, `don't want`, `always use`, `never use`, `please use`, or
  `do not want`, and only from the dropped window.
- Compaction still clears the conversation's undo records: the message indexes
  they point at no longer exist. This is unchanged from ADR-0002, and now
  happens automatically mid-conversation.
- Auto-compaction needs a catalog entry for the model. Private or brand-new
  models without a models.dev window never auto-compact; `/compact` still works.

## Revisit when

- A dedicated compaction model is wanted (provider/model/effort override like
  the title generator), or the window-scaled threshold curve (90% at ≤32k down
  to 40% at 1M) replaces the flat percentage.
- Persisting the last usage into the conversation file is worth the schema
  change (removes the estimate from the restart path).
- Append-only compaction segments are wanted, so an old summary is never
  rewritten by a later one.
- Tool-output recall (`#N:path` style) lands; the brief could then omit output
  with a marker instead of losing it outright.

## Out of scope

`/undo` after an automatic compaction, a per-conversation threshold override,
compacting subagent runs, and migrating existing transcripts to the new summary
shape.
