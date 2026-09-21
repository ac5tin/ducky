# ADR-0003: Prepend the login shell's PATH to stdio MCP server spawns

- **Status:** Accepted
- **Date:** 2026-09-21

## Context

Stdio MCP servers are commands like `npx -y @modelcontextprotocol/server-memory`
spawned by `McpManager::build_transport` (`src-tauri/src/mcp/manager.rs`). The
child inherits the Ducky process's environment verbatim — including `PATH`.

When Ducky is launched from a terminal, that `PATH` is the shell's. When it is
launched from the desktop (the packaged, auto-updating app), the process
inherits the GUI session's environment instead: on Linux that is the systemd
user manager's `PATH` (roughly `/usr/local/bin:/usr/bin:/bin`), on macOS
whatever the launch agent provided. Version managers — nvm, fnm, volta,
Homebrew-installed Node on macOS — only extend `PATH` in shell profile files,
which GUI launches never source. On such machines a bare `npx` fails at spawn
with ENOENT ("Could not start `npx` … No such file or directory"), even though
the same server works from every terminal.

The interactive PTY (`src-tauri/src/terminal.rs`) already solves this for
itself by launching `$SHELL -l`; the MCP spawn path did not.

## Decision

For stdio server spawns (and only those — HTTP transports and the `git`
snapshots in `snapshot.rs` are untouched), prepend the login shell's `PATH`:

- Source the login `PATH` once per process by running
  `$SHELL -l -c 'printf %s "$PATH"'` (fallback shell `/bin/sh`), cached in a
  `OnceLock`. On any failure (no shell, non-zero exit, no `/` in the output)
  we keep the inherited environment unchanged.
- Set the child's `PATH` to `<login PATH>:<inherited PATH>` — login-first so
  the user's own toolchain wins, inherited appended so nothing visible today
  becomes unresolvable.
- Unix only (`#[cfg(unix)]`). Windows GUI launches get the registry user
  `PATH`, which installers update; wrapping `npx.cmd` for Windows is a
  separate problem.

The one-off shell spawn (~tens of ms) happens lazily on the first stdio
connect, not at startup, and is amortised across all servers.

## Consequences

- Desktop-launched Ducky resolves `npx`/`uvx`/`bunx` exactly as the user's
  terminal does, without a settings toggle.
- A user whose profile *narrows* `PATH` gets it narrowed for MCP servers too;
  we accept this because the alternative (GUI `PATH`) is strictly less
  capable, and the user can still pin absolute paths per server.
- Sourcing profiles executes user shell config; this is the same trust
  posture as the built-in terminal, which already runs a login shell.
- A `PATH` set in a server's per-server env map wins: `build_transport`
  skips the augmentation entirely when the map already provides one, so
  explicit config beats inference.
