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

## Consequences

- All four providers work today with the existing OpenAI-compatible adapter
  and the existing `provider_keys` secrets map. No new wire protocol, no new
  secret storage, no new auth code.
- OpenCode Go models that the provider serves only on `/responses`
  (Grok 4.7/4.6, GPT 6 Luna, GPT 5.6 Luna) are not reachable yet; the provider
  returns its own error for those model ids and the other 37 models work.
- Ducky does not send `x-opencode-session`. The provider calls it a preference
  ("your client should"), and the provider layer has no conversation id in
  `ChatOptions` to route it from.
- Alibaba reasoning is not forced on: the docs enable it with
  `extra_body.enable_thinking`, which the OpenAI adapter does not send. Users
  who pick an effort level still get `reasoning_effort`.
- ChatGPT subscription access stays out of reach, so OpenAI remains
  pay-per-token through an API key.

## Revisit when

- OpenAI publishes a third-party OAuth registration, or a Ducky-registered
  client is approved.
- xAI documents partner OAuth for non-Grok-CLI clients, or the user accepts
  the Grok CLI client explicitly.
- OpenCode Go's `/responses`-only models become the models users ask for; that
  needs a Responses adapter, which is a separate decision from auth.
- Qwen Token Plan or OpenCode Go report abuse-routing problems that the
  session header would fix.
