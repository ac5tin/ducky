# ADR-0007: Plugin packages are the source of truth

- **Status:** Accepted
- **Date:** 2026-10-06

## Context

A plugin ships skills, MCP servers and subagents inside one package directory
at `<app_data>/plugins/<plugin-id>/package` (design:
`docs/superpowers/specs/2026-10-01-plugins-and-marketplaces-design.md`). The
app already stores user-owned MCP servers in `AppConfig.mcp_servers` and
subagents in `AppConfig.subagents`, so plugin components could be copied into
those lists when the plugin is enabled.

Copying is the intuitive choice: every existing consumer — the Connectors
view, the MCP manager, the subagent registry — would keep reading one list and
need no change. It fails on the write side. An update or a rollback replaces
the package directory; a copy would then have to find and reconcile every
materialised entry, and an uninstall would have to delete entries it can only
recognise by an id prefix. Two records would claim ownership of the same
component, and a crash between the two writes would leave them disagreeing.

## Decision

The package directory is the source of truth for plugin components.
`AppConfig` holds policy only — the four `PluginSettings` fields — and never a
copy of a plugin's skills, servers or subagents.

- A plugin's components are read from the package by the `PluginManager` on
  every reload, and merged into the read path: `McpManager` merges
  `PluginIndex.servers` into the resolved server list, `resolved_skills`
  collects skills across the four roots, and the subagent registry merges
  plugin subagents.
- Per-plugin state lives on the install record (`enabled`, `disabled_servers`,
  `updatePolicy`, `previousVersion`, `treeHash`), written by one `save` under
  one mutex.
- Plugin MCP servers are namespaced `plugin:<plugin-id>:<server>` and are
  never in `AppConfig.mcp_servers`, so `mcp_update` and `mcp_remove` cannot
  touch them; Connectors routes their enable switch to
  `plugin_set_server_enabled`.
- Two records in `<app_data>/plugins/` (`installed.json` and the marketplace
  store) are the complete durable state of an install.

## Consequences

- An update or rollback is a directory swap. No config migration, no entry
  reconciliation, and nothing to clean up if the swap fails.
- A component cannot be edited in Connectors: the row is read-only and points
  at Plugins. This is a deliberate cost of one owner per component.
- The read path resolves the merged view on demand, so enable/disable takes
  effect with no restart and no write to `AppConfig`.
- Secrets stay out of plugin config by construction: a plugin's `env` and
  header values live in its package and reach a spawn, never `secrets.json`
  and never the webview.
- Deleting the app data directory removes every plugin artifact; deleting a
  plugin cannot leave a stale server behind in `AppConfig`.

## Revisit when

- A component must exist without a package directory, for example a plugin
  whose servers a user wants to customise in place.
- The merged read path measurably costs on every list call.
- A second consumer needs plugin components outside the merged views and
  starts duplicating the merge logic.
