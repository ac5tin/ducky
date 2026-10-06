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
- Entry fields are read as a superset of the Claude Code and ZCode shapes.
  Unknown fields are ignored, including `dependencies`: `parse_entry` reads no
  dependency field and install never checks one, so an entry that names
  dependencies installs exactly as if the field were absent. (Cross-marketplace
  dependencies are out of scope in the design.)
- An unsupported `source` kind (`npm`, `archive`, `command`) produces an
  unavailable entry with the reason, never a failed registry.
- `entries[].defaultEnabled` is deliberately not parsed (ruling R13).
  Honouring the Claude Code hint would let a registry influence what runs on
  install, which would contradict ADR-0009: install writes `enabled: false` and
  leaves every owned server in `disabled_servers`. The rejected alternative was
  to read it as an enable hint or as the update-policy hint; the update policy
  instead derives from the package content (`derive_policy`) with the user's
  `PluginSettings.policy_default` (Content/Auto/Manual) as the only override.
  Consequence: an entry that sets `defaultEnabled` installs disabled under the
  content-derived policy, exactly like an entry that omits the field.
- A failed refresh is recorded on the marketplace record and the previous
  snapshot is kept, so a network outage cannot empty the catalog.

## Consequences

- A Claude Code, ZCode or Copilot CLI marketplace installs from without a
  conversion step.
- Ducky-specific behaviour (extra entry fields) must be additive: a registry
  written for another client keeps working, and a Ducky registry that uses only
  Ducky fields is still valid.
- The normalised `Entry` carries `name` and `display_name` (`marketplace.rs`),
  with no third name field. `name` is the registry key a marketplace install
  selects by and `display_name` is the label the UI shows; the install id comes
  from the package manifest's own `name` (`assign_id` slugs it for the Claude
  layout and uses it unchanged for the Agent Plugins layout), never from the
  entry.
- The probe order is a compatibility contract. A repository that ships two
  registries gets the Claude Code one; moving a file between the three paths
  can change what a marketplace lists.

## Revisit when

- The ecosystem converges on one registry path and one source vocabulary.
- A registry dialect needs field semantics Ducky cannot express additively,
  for example cross-marketplace dependencies.
- A registry is found where the probe order picks a file the user did not
  mean, and an explicit per-marketplace path is worth the UI.
