# ADR-0001: Silence rmcp SEP-2577 deprecations

- **Status:** Accepted
- **Date:** 2026-09-18

## Context

`rmcp 3.1.4` marks MCP Sampling, Roots, and Logging types as deprecated by
[SEP-2577](https://github.com/modelcontextprotocol/modelcontextprotocol/pull/2577).
In this version, these types are still the `ClientHandler` and MRTR APIs needed
to support older MCP servers. A local `tauri dev` build produced 51 warnings.

Removing these features now would reduce compatibility. Leaving the warnings
visible also makes normal development output noisy.

## Decision

Keep Sampling, Roots, and Logging support for backwards compatibility. Silence
the deprecation lint only at the compatibility call sites:

- `src-tauri/src/mcp/handler.rs`: the deprecated `ClientHandler` surface.
- `src-tauri/src/mcp/bridge.rs`: Sampling imports, execution, conversion, and test.
- `src-tauri/src/mcp/manager.rs`: Roots imports and MRTR input handling.

Do not use a crate-wide allowance, global `RUSTFLAGS`, or runtime logs for this
compile-time decision.

## Fixed versus silenced

- Fixed: changed `rand::thread_rng` to `rand::rng`.
- Fixed: removed the unused `ProtectedResourceMetadata.resource` field.
- Silenced for later review: the rmcp Sampling, Roots, and Logging deprecations.

## Consequences and risk

Development builds are quiet and older MCP servers continue to work. The
allowances also hide future deprecation messages at these sites. A future rmcp
release that removes the types will still cause a compile failure and require a
migration.

## Revisit when

Revisit this decision when any of these conditions occurs:

- rmcp provides non-deprecated replacements for the affected APIs.
- rmcp removes the deprecated types, or `rmcp` has a major version bump.
- Ducky drops support for pre-SEP-2577 MCP servers.

Migration must remove the allowances, update the handler and MRTR paths, and
run the Rust checks and tests before the decision is changed.

## Out of scope

Using the OAuth protected-resource `resource` value for RFC 8707/9728
validation is a separate decision. Crate-wide lint suppression and runtime
logging remain rejected alternatives.
