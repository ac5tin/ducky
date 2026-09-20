# ADR-0002: Slash commands (/compact, /undo, /init)

- **Status:** Accepted
- **Date:** 2026-09-20

## Context

Ducky's composer only sent plain chat text. Users wanted opencode-style slash
commands: `/compact` (shrink context), `/undo` (drop the last turn **and**
revert its file changes), and `/init` (create/update `AGENTS.md`). Any design
had to fit the existing model: history lives only in
`conversations/<id>.json` and is reloaded per turn, file writes flow through
the builtin `ducky__fs_*` tools (but MCP servers can also write files), the
working directory is a single global setting, and it is *not* guaranteed to be
a git repository.

## Decision

### Command interception (frontend)

Commands are parsed in the store's `send()` (`src/store.ts`), *before* the
busy-queue branch, so a command is never queued as chat text. Parsing lives in
`src/slashCommands.ts`: only a bare first token counts
(`/^\/[A-Za-z][\w-]*$/`), so pasted paths like `/home/x` still send as normal
messages; unknown command-like tokens are refused with a toast. Typing `/`
opens an autocomplete popup (`SlashCommandMenu.tsx`) with arrow/Tab/Enter/Esc
handling.

### /init — a canned prompt, no backend

`/init` expands a template (adapted from opencode's `initialize.txt`) into an
ordinary user message that the model answers with its existing file tools.
No app-side repository scan. In the transcript the template renders collapsed
(`Ran /init` chip, detected by the `[/init]` first line).

### /compact — summary carried by a marked user message

`conversation_compact` runs one provider call (the `stream_title` pattern)
with an opencode-style structured summarisation prompt, then replaces the
whole history with a single `Msg::User` starting with the
`[Conversation compacted]` marker (`src-tauri/src/compact.rs`). The marker —
not a new message kind — is what the UI special-cases, what a later `/compact`
extracts as `<prior-summary>`, and what `/undo` skips when finding the turn
boundary. Compaction clears the conversation's undo records (their message
indexes no longer exist).

### /undo — hidden git snapshot repos (opencode's mechanism)

`src-tauri/src/snapshot.rs` keeps one private git repo per project at
`<app_data>/snapshots/<hash-of-repo-root>/`, addressed on every invocation
with `--git-dir/--work-tree` (env vars like `GIT_DIR` are cleared). At init it
borrows the project's object database via `objects/info/alternates` and copies
the project index, so captures only store what changed. The user's own repo is
never touched.

`chat_send` captures a tree (`git add -A` + `write-tree`, on a blocking task)
before spawning the turn and persists an `UndoRecord { user_index, tree, wd }`
into the conversation file's `undo` array immediately (`user_index` = the
index the new user message will land at). `conversation_undo` finds the last
non-marker user message, restores the recorded tree (diff-tree against a fresh
capture; modified/deleted files are checked out, files added since are
deleted, emptied parent dirs pruned), truncates messages and records, and
returns the removed prompt for the composer.

Outside git repositories (or without git) capture is skipped: `/undo` still
removes the messages and warns that files were not reverted — the degradation
opencode also has, made explicit instead of silent.

### Race guards

A `/compact` must never interleave with a running turn (both rewrite the
conversation file). `chat_send`, `conversation_compact`, and
`conversation_undo` check-and-insert under the nested lock order
`compacting` → `runtimes`, taken consistently, which serialises all three.

## Consequences and risk

- Gitignored files are never snapshotted and never reverted (same as
  opencode). If a turn edits `.gitignore` itself, restore-time ignore rules
  may differ from capture-time.
- The working directory is global: concurrent chats in the same repo share one
  snapshot repo, so undoing a turn in one conversation can revert another
  conversation's newer file changes. Inherent to the shared-directory model.
- Snapshot repos only grow (one blob set per changed file per turn); no size
  cap or `git gc` yet.
- `Msg` gained no variant; older transcripts simply have no `undo` array
  (serde default), and older builds ignore the extra key.
- The `add -A` capture runs on every `chat_send` in a git repo, even for
  pure-chat turns; on very large repositories this is measurable.

## Revisit when

- Auto-compaction at a token threshold (persisted usage) is wanted.
- `/redo` is wanted (it needs the post-turn snapshot opencode stashes).
- Capture cost matters enough to move snapshots to the first mutating tool
  call of a turn, or a blob size cap / periodic `git gc` is needed.
- Per-conversation working directories land (records then key per repo as
  today, but the shared-repo caveat disappears).

## Out of scope

Custom user-defined commands, MCP prompt commands, and non-git file-revert
fallbacks (e.g. recording pre-write contents at the `ducky__fs_write` choke
point) were considered and declined for v1.
