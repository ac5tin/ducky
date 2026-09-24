# ADR-0006: New providers ship with API-key sign-in; provider OAuth is deferred

- **Status:** Accepted
- **Date:** 2026-09-24

## Context

Ducky was asked for four more providers — OpenCode Go, Qwen Token Plan,
CommandCode and xAI (Grok) — and for OAuth sign-in instead of an API key where
the provider supports it (specifically xAI and OpenAI/ChatGPT).

What the official documentation supports, checked 2026-09-24:

- **OpenCode Go** — API key from OpenCode Zen; base URL
  `https://opencode.ai/zen/go/v1`; `GET /models` documented. Models are split
  across `/chat/completions` and `/responses`; the docs ask clients to send a
  stable `x-opencode-session` header per conversation.
- **Qwen Token Plan** — API key (`sk-sp-…`); OpenAI-compatible base URL
  `https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1`
  (China/Beijing region only). Models are listed in the docs, not by a
  documented endpoint.
- **CommandCode** — API key; `https://api.commandcode.ai/provider/v1` with
  `/chat/completions` and `GET /models`.
- **xAI** — API key; `https://api.x.ai/v1`. Its OpenID metadata
  (`https://auth.x.ai/.well-known/openid-configuration`) advertises a
  device-authorization grant and refresh tokens, but the only known client is
  a **Grok CLI** client, not a Ducky registration.
- **OpenAI/ChatGPT** — OpenAI documents ChatGPT sign-in for **Codex clients**.
  There is no documented OAuth flow for third-party applications such as
  Ducky, and no Ducky-registered client exists.

An open-source client (OpenCode) implements both flows. Its ChatGPT flow is
Codex client impersonation — a Codex client id, `originator: "opencode"` and
`https://chatgpt.com/backend-api/codex/responses`. Reusing those values would
present Ducky as another vendor's client. Its xAI flow uses a public Grok-CLI
client with a `grok-cli:access` scope that is not Ducky's grant.

## Decision

The four new providers ship as **API-key connections only**. No OAuth flow is
added for xAI or OpenAI at this time, and no third-party client id, originator
value, callback port, cookie or endpoint is copied into Ducky.

Chosen by the user as option 1 of three, on 2026-09-24.

When provider OAuth is added later, it must:

- live in its own module, not in `src-tauri/src/oauth.rs`, which implements the
  MCP authorization spec (RFC 9728 resource metadata, `resource` indicator,
  MCP handshake) and is not a provider sign-in flow;
- store tokens in their own secrets map, not in `Secrets.oauth_tokens`, which
  is keyed by MCP server id;
- use a Ducky-registered client for OpenAI, or a documented
  third-party-authorized client for xAI — not another application's;
- keep **one auth method per connection**: an API key and an OAuth session are
  separate provider entries, never merged on one record.

A second decision rides along because OpenCode Go does not work without it:
OpenCode's relay rejects a request that carries no session id
(`400 MissingSessionID`), so every request that belongs to a conversation now
sends `x-opencode-session: <conversation id>` and `x-opencode-client: ducky`.
The conversation id is passed through `ChatOptions::session_id` and sent only
to `opencode.ai` hosts, never to other providers. The same request path also
identifies Ducky with a real `User-Agent` (`ducky/<version>`) instead of the
HTTP library's name, which OpenCode asks of its clients.

## Consequences

- All four providers work with the API key and the existing `provider_keys`
  secrets map. No new secret storage and no new auth code.
- OpenCode Go is usable: title generation, `/compact` and MCP sampling carry
  the conversation's session id too, so no request falls into the
  `MissingSessionID` error. Only one session id is used per conversation, which
  is what the provider asks for so its prompt cache stays warm.
- Other providers never see the session header. The host check rejects
  lookalike hosts such as `opencode.ai.example.test`.
- OpenCode Go serves its catalogue over three wires, so one connection can now
  reach every model. The wire is resolved per model, not per connection:
  models.dev publishes a package per model (`@ai-sdk/openai` → `/responses`,
  `@ai-sdk/anthropic` → `/messages`, otherwise `/chat/completions`), and that
  map is followed **only** for `opencode` and `opencode-go`. models.dev
  declares a package for every provider, so following it everywhere would move
  OpenAI, xAI and OpenRouter traffic off `/chat/completions`.
- The `/messages` wire reads a different auth header per gateway: `x-api-key`
  for Anthropic and OpenCode, `Authorization: Bearer` for CommandCode. Both are
  verified by probing each endpoint with a deliberately invalid key and reading
  which header the error names.
- CommandCode's 9 Claude models work: `claude-*` routes to `/messages` with a
  bearer token, matching what the reference client does. The other 72 models
  stay on `/chat/completions`. The earlier picker filter is gone, since the
  models are reachable now.
- Alibaba needs no `enable_thinking`: its hybrid Qwen3.5/3.7/3.8 models enable
  thinking by default, so the parameter is only ever used to turn it off. The
  adapter still sends `reasoning_effort` when the user picks a level.
- ChatGPT subscription access stays out of reach, so OpenAI remains
  pay-per-token through an API key.

## Revisit when

- OpenAI publishes a third-party OAuth registration, or a Ducky-registered
  client is approved.
- xAI documents partner OAuth for non-Grok-CLI clients, or the user accepts
  the Grok CLI client explicitly.
- A model resolves to the wrong wire. models.dev lags a gateway's own model
  list, and a cold catalog falls back to `/chat/completions`; the CommandCode
  rule is a model-id prefix, not a published list.
- Another gateway starts serving several wires. The gate in
  `serves_several_wires` is the one place to extend, after checking its auth
  header and that following models.dev will not move its other models.
- The Responses adapter is fixture-tested only. It has no live 200 yet, because
  it needs a real key; the first user with one should confirm text, tool calls
  and usage against OpenCode Go's Grok or GPT models.
- Qwen Token Plan or OpenCode Go report abuse-routing problems that the
  session header would fix.
