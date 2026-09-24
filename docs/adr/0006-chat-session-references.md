# ADR-0006: Chat references are id-backed and read on demand

- **Status:** Accepted
- **Date:** 2026-09-24

## Context

The composer lets a user point the model at an earlier chat, modelled on
Zcode 3.14.3's `#` session mentions (design:
`docs/superpowers/specs/2026-09-24-chat-session-references-design.md`). Two
shapes are possible: paste the referenced transcript into the current request,
or keep a stable binding and read on demand.

Pasting pays tokens on every request whether or not the history matters, and it
mixes another conversation into the current user message. A title-based binding
is ambiguous — titles are editable and not unique.

Old chat text is untrusted input, and its tool output can be very large.

## Decision

The composer inserts an id-backed token (`#chat_<conversation-id>`). At request
time the engine adds a model-only system note listing the referenced ids and
pointing at a read-only built-in tool, `ducky__read_session_context`. That tool
returns only user and assistant text, in transcript order, capped at 24,000
characters, and only for ids present in `config.conversations` — the file path
is always derived from the stored id, never from the argument. The note is
never persisted; the saved transcript keeps the user's text exactly as typed.

The tool is `read_only: true`, so the existing `mode_allows` predicate offers
and permits it in ReadOnly and Plan modes, and it rides the normal approval
pipeline.

## Consequences

- Token cost is paid only when the model needs the referenced chat.
- A deleted chat yields a clean "no saved chat" error, not stale content.
- Tool payloads, system prompts and model-only notes never leave the source
  transcript or enter the current one.
- No transcript-secret scrubbing: user and assistant text is returned as
  written, the same trust level as opening that chat in its own window.
