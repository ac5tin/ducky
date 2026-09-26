# ADR-0007: The advisor tool is a client-side tool, exempt from the approval gate

- **Status:** Accepted
- **Date:** 2026-09-26

## Context

The advisor pattern pairs a faster executor model with a stronger reviewer
model that the executor consults mid-task. Design:
`docs/superpowers/specs/2026-09-26-advisor-tool-design.md`.

Anthropic ships this pattern in two incompatible shapes:

1. **Server-side tool.** `{"type": "advisor_20260301", "name": "advisor",
   "model": ...}` in the `tools` array plus the `advisor-tool-2026-03-01` beta
   header. Anthropic runs the advisor inference inside the same
   `/v1/messages` request. Limits: Anthropic API only (not Bedrock, Google
   Cloud, or Microsoft Foundry), a model capability pairing table that returns
   `400` on an invalid pair, no reasoning-effort control for the advisor, and —
   with an Opus 5 advisor — the advice is returned as an opaque
   `advisor_redacted_result.encrypted_content` blob that the client may
   round-trip but never read.
2. **Client-side tool.** A zero-parameter tool. On call, the host makes a
   separate request: advisor system prompt, the tool inventory, the resolved
   transcript, `tools: []`, and a configured reasoning effort. Pi's built-in
   `advisor` and `@juicesharp/rpiv-advisor` work this way.

Ducky talks to four provider kinds (OpenAI-compatible, Anthropic, and the
xAI / OpenCode Go / Qwen Token Plan / CommandCode presets), and the requested
feature includes per-conversation advisor model *and* reasoning-effort pickers.

Two further questions had to be settled either way: whether an advisor call
needs per-call user approval, and whether the advisor may run inside subagents
and inside the read-only modes.

## Decision

**Client-side tool.** Ducky registers `ducky__advisor` as a built-in, zero-argument,
`read_only: true` tool. The engine assembles the advisor request from the
caller's live history and executes it as a separate provider call to the
configured advisor model, on whichever provider the user picked. The advice
text becomes the tool result, so it is always readable in the transcript.

Ducky does **not** attach the Anthropic-native server tool, even when the
active provider is Anthropic.

**No approval prompt.** The advisor branch is dispatched before the consent
gate, like the control tools. Enabling the advisor for a conversation is the
consent: the user turned it on, the call cannot touch the workspace or remote
state, and the transcript shows the consult and the advice. Gating it per call
would instead block a running turn on a click at exactly the moment the
executor has decided it needs help.

**Available wherever the mode and the tool allowlist permit.** `read_only: true`
puts the advisor under the existing `mode_allows` predicate, so ReadOnly and
Plan mode offer it (it cannot write). It is not a control tool, so subagents
get it too, subject to their allowlist, and it consults about the subagent's own
branch.

## Consequences

- The advisor works for every provider Ducky supports, and the advice is always
  readable — no encrypted results.
- Adopting the tool costs one extra billed model call per consult, on the
  advisor model's rates, and re-sends the whole transcript each time. The
  `max_tokens: 2048` bound on the advisor call limits the output side.
- The transcript is forwarded to the advisor's provider, which the user may set
  to a **different** provider from the chat. This is a new data-egress path, and
  the first Ducky feature that sends a conversation to two endpoints. The user
  opts in per conversation, and the header chip names the advisor model.
- The advisor's tokens are not reported in Ducky's usage display.
- Provider prompt caching is unaffected in the sense that no history is
  rewritten, but the request `tools` array changes when the advisor toggles, so
  the cached prefix is invalidated once per toggle.
- Approval-exempt network calls are a category the approval gate no longer
  covers end to end. Any future tool in this position should be added to the
  same early-dispatch branch deliberately, and it should have no side effects
  outside its own provider call.

## Revisit when

- Anthropic's advisor tool becomes worth exposing directly: plaintext results
  for the models users actually pick, providers beyond the Anthropic API, or
  effort control. The client-side tool can coexist with a native path that is
  enabled only for valid Anthropic pairs.
- Advisor cost reporting matters enough to surface per-call usage in the card or
  the usage chip.
- A user objects to the transcript reaching a second provider — the mitigation
  would be restricting the advisor to the chat's own provider, or a per-call
  confirmation.
