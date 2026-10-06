# ADR-0008: Marketplace registry compatibility

- **Status:** Accepted
- **Date:** 2026-10-06

## Context

A marketplace is a repository that lists installable plugins in a registry
JSON file. Claude Code uses `.claude-plugin/marketplace.json`; ZCode uses a
root `marketplace.json`; Ducky's own design adds `.ducky/marketplace.json`.
The registries overlap but not completely: they disagree on where the file
lives, how a plugin `source` is written, and which entry fields exist.

The tempting alternative is to support only Ducky's own path and shape, or to
require users to convert an existing registry before adding it. Both reject
the marketplace a user already has, and the whole point of a marketplace is
that a known public repository installs without conversion.

## Decision

Read the ecosystem dialects and normalise them into one internal model.

- Probe in this order and stop at the first file that exists:
  `.claude-plugin/marketplace.json`, then `marketplace.json`, then
  `.ducky/marketplace.json`.
- A `source` may be `"./relative/path"`, `github`, `git`, `git-subdir`,
  `directory`/`file`, `url` (git over HTTPS), or the `owner/repo` / bare git
  URL shorthand used by marketplace sources.
- Entry fields are read as a superset of the Claude Code and ZCode shapes;
  unknown fields are ignored, and known-but-unimplemented dependencies are
  reported as unsupported rather than as errors.
- An unsupported `source` kind (`npm`, `archive`, `command`) produces an
  unavailable entry with the reason, never a failed registry.
- `entries[].defaultEnabled` is honoured only as the installed plugin's update
  policy hint — Ducky still installs every plugin disabled (ADR-0009).
- A failed refresh is recorded on the marketplace record and the previous
  snapshot is kept, so a network outage cannot empty the catalog.

## Consequences

- A Claude Code, ZCode or Copilot CLI marketplace installs from without a
  conversion step.
- Ducky-specific behaviour (policy hints, extra entry fields) must be
  additive: a registry written for another client keeps working, and a Ducky
  registry that uses only Ducky fields is still valid.
- The normalised `Entry` carries `raw_name` next to `name`, so an entry whose
  display name is not a valid plugin id can still be installed under a
  derived id.
- The probe order is a compatibility contract. A repository that ships two
  registries gets the Claude Code one; moving a file between the three paths
  can change what a marketplace lists.

## Revisit when

- The ecosystem converges on one registry path and one source vocabulary.
- A registry dialect needs field semantics Ducky cannot express additively,
  for example cross-marketplace dependencies.
- A registry is found where the probe order picks a file the user did not
  mean, and an explicit per-marketplace path is worth the UI.
