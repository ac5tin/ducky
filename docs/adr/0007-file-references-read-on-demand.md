# ADR-0007: File references are read on demand

- **Status:** Accepted
- **Date:** 2026-09-29

## Context

The composer lets a user point the model at a file with an `@path` token,
modelled on the `#` chat references (ADR-0006) and on the `@` mention menus of
pi and Zcode. Two shapes were possible: paste the file contents into the
request, or keep the token and read on demand.

Pasting pays tokens on every request whether or not the file matters, needs
clamping for large and binary files, and bloats the saved transcript with
duplicated content.

## Decision

The composer inserts the path as typed (`@relative/path`, quoted when it
contains spaces). A new `fs_suggest` command feeds the autocomplete menu; it
walks only the working directory, uses the same skip list and depth cap as the
filesystem tools, and is UI-only — the model never sees it. At request time
the engine adds a model-only system note listing the referenced paths and
pointing at the existing `ducky__fs_read`. That tool stays sandboxed to the
working directory, byte-capped, and binary-rejecting; it is `read_only: true`,
so `mode_allows` offers and permits it in ReadOnly and Plan modes and the
normal approval pipeline applies. The note is never persisted; the saved
transcript keeps the user's text exactly as typed.

## Consequences

- Token cost is paid only when the model reads the file.
- No new tool, no new trust boundary: reads go through the existing
  containment check in `ducky__fs_read`.
- `ducky__fs_search` no longer returns `SKIP_DIRS` or dot-directories as hits,
  because it now shares one walk with `fs_suggest`, and that walk neither
  lists nor enters noise directories. Deep matches beyond the 1,000-entry walk
  cap can be missed; raise the cap if that ever bites.
- A directory row in the menu inserts `@dir/ `, which is not a readable file;
  `ducky__fs_read` returns a clean error for it.
