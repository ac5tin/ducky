# ADR-0009: Plugin trust model

- **Status:** Accepted
- **Date:** 2026-10-06

## Context

Installing a plugin means running third-party code: a skill can instruct the
model, a subagent can spawn with tools, and a stdio MCP server starts a local
program with the user's account. Remote servers receive requests and can
redirect them.

The ecosystem takes two positions. ZCode enables a plugin on install, so the
first execution happens without a decision. Claude Code shows a trust action
before enabling and warns that the plugin runs with the user's account. A
third option — enable everything and rely on an OS sandbox — would need a
platform sandbox that this application does not have (it is listed as out of
scope in the design).

The project already has a stated posture for MCP: tool annotations are
untrusted hints, consent is per call with allow-list memory, and the one
sanctioned exception is the `readOnlyHint` mode gate in ADR-0004.

## Decision

Match Claude Code's posture, with two explicit levels, and write the residual
risk down instead of implying a guarantee.

- Install never executes and never connects anything. The record is written
  with `enabled: false`, and the install sheet shows everything that will run
  — every skill, subagent, and for each server its transport with the exact
  `command` and `args` or the host — before the enable action.
- Enable is two levels. `plugin_set_enabled` turns the plugin on so its skills
  and subagents exist. Every plugin MCP server has its own switch, **default
  off**; turning one on records a one-time consent for that server and then
  connects it through the normal `McpManager` lifecycle.
- The enable warning is shown once and is position-independent: *enabling a
  plugin runs its code with your user account; MCP servers start local
  programs and reach remote hosts*.
- MCP approvals are unchanged: plugin tools ride the normal per-call consent
  and allow-list memory. Tool annotations stay untrusted, except that the
  `readOnlyHint` gate of ADR-0004 applies to plugin tools exactly as it does
  to user servers.
- Path containment is enforced at every package path (design §13): a manifest,
  component directory, skill file, `command` or `cwd` that resolves outside
  its root is refused with the narrowest applicable failure boundary.
- Secrets stay out of plugin configuration: a plugin's `env` and header values
  are read from the package at spawn and never enter `secrets.json` or the
  webview, and reserved environment names (`PLUGIN_ROOT`, `PLUGIN_DATA`) are
  set by Ducky and refused in the manifest.
- Updates re-validate exactly as install does. A package modified locally is
  detected by its tree hash, skipped by automatic updates, and a manual update
  asks before overwriting. Rollback restores the previous revision.

## Consequences

- A plugin that is never enabled cannot execute anything. This is the main
  protection, and it is the user's decision rather than a sandbox's.
- There is no sandbox. A plugin that is enabled and whose server is switched
  on runs as the user, with the user's network access and filesystem
  permissions; Ducky can contain paths it resolves itself, not the behavior of
  the program it launches.
- The UI must keep the two levels visible — a plugin is not "on" merely
  because a server is — and the trust sheet must be accurate, or the consent
  is not informed.
- Every plugin tool call is a normal approval prompt. A user who approves
  broadly is not protected by annotations; the transcript and logs are the
  record.

## Revisit when

- An OS-level sandbox becomes available and worth the platform work; the
  two-level consent stays valid underneath it.
- A plugin is found abusing path containment or the approval bypass for
  `readOnlyHint`; both have named tests and one predicate each.
- The ecosystem settles on signed plugins or a registry attestation, which
  would add a verification level above consent.
