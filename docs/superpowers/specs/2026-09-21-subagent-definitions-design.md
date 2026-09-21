# Configurable Subagents — Design

Date: 2026-09-21
Status: implemented (this document records the design as built; see the
2026-09-21 addendum at the bottom for the card-identity follow-up)

Builds on `2026-09-20-subagents-design.md` (the spawn engine). Reference
research: ZCode's agent system — one Agent tool with a `subagent_type`
parameter, agent definitions carrying name / when-to-use description /
system prompt / optional model + effort overrides / optional tool allowlist,
managed in Settings.

## Goal

Subagents become persistent, user-editable definitions instead of anonymous
spawns. The settings page gains a Subagents section listing, adding, editing
and deleting them. Ducky ships two defaults — **General-Purpose** (every
tool, no persona beyond the generic preamble) and **Explore** (read-only
searcher) — both inheriting the chat's model and effort out of the box.

## Decisions

- **One tool + `agent` parameter** (ZCode's shape, not one tool per agent).
  With at least one subagent configured, `ducky__subagent`'s definition is
  rebuilt per round (`builtin::subagent_tool_def`) from the config: the static
  schema plus an `agent` string enum whose description carries each type's
  name and when-to-use line. With zero configured, the static generic
  definition is used unchanged.
- **Defaults are seeded, not merged.** On first load (or first run after
  upgrading) `Store::new` inserts General-Purpose and Explore into
  `config.json` and sets `subagents_seeded`. From then on the list is
  entirely the user's — deleting everything stays deleted. Settings offers
  "Restore defaults", which re-inserts any default whose name is missing
  (case-insensitive) and never overwrites existing entries or user edits.
- **Inheritance by default.** `provider_id`/`model`/`effort` overrides are
  optional; unset means "whatever the conversation uses", exactly the
  pre-existing behavior. A model override requires a provider. At spawn time
  the definition is resolved into an owned `SubagentSpec` snapshot, so config
  edits mid-run cannot change a running subagent. A stale provider override
  (provider deleted) silently falls back to full inheritance, dropping the
  model override with it.
- **Prompt layering.** Subagent system message = generic preamble (isolated
  context, autonomous, final answer) + cwd grounding, then for a typed spawn
  `# Your role\nYou are "{name}": {description}` + the definition's
  `system_prompt` + (when restricted) the tool-allowlist notice. General-
  Purpose ships with an empty `system_prompt`, so its prompt is byte-for-byte
  the old one.
- **Tool allowlists are enforced, not suggested.** Entries match an exact
  tool name (`ducky__fs_read`), a whole server (`server_id` or
  `server_id/*`), or a qualified tool (`server_id/tool`). Enforcement is
  two-layer: the filtered tool list means the model never sees unallowlisted
  tools, and `resolve`-time guards in `execute_tool`/`prepare_subagent`
  reject any attempt anyway. Explore's allowlist is the read-only builtins
  (fs list/read/search, web fetch/search) — no writes, no nesting.
- **Unknown agent fails fast.** A spawn naming an unconfigured type errors
  before the consent prompt, listing the available names so the model can
  self-correct.
- **Trust posture unchanged.** The spawn tool stays `read_only: false`;
  every call still goes through the approval gate; a subagent's own tool
  calls ride the same per-call approvals. Allowlists narrow what a subagent
  may attempt; they never widen anything.
- **Card identity.** The `agent` argument rides in the persisted tool-call
  args, so replayed transcripts render. `subagentHeader` falls back
  `name → agent → "Subagent"`. The running card's `SubagentMeta` chips show
  the *resolved* provider/model/effort (overrides or inherited).

## Surface

- Config: `SubagentConfig` in `config.rs` (`id`, `name`, `description`,
  `system_prompt`, `provider_id`, `model`, `effort`, `tools`, `created_at`);
  `AppConfig.subagents` + `subagents_seeded`, both `#[serde(default)]`;
  `validate_subagent` (name ≤64 unique case-insensitive, description
  required ≤1000, model ⇒ provider, provider must exist, model in the
  provider's list when cached, no blank allowlist entries).
- Commands: `subagent_add` / `subagent_update` (full replace, `created_at`
  preserved) / `subagent_remove` / `subagent_restore_defaults`, registered in
  `lib.rs`.
- Engine (`agent.rs`): `SubagentSpec` carried in `RunScope::Subagent`;
  `tool_allowed` matcher; spec-aware tool list build; per-round effort =
  `spec.effort.or(conversation effort)`; `resolve_spec_model` shared by
  `run_subagent` and `subagent_meta`.
- Settings UI: "Subagents" section (rows with model/effort/tools chips, edit,
  delete, add, restore defaults) and `SubagentEditorModal` (name,
  description, system prompt, provider+model with an "Inherit from the chat"
  option, effort pills, tool restriction as checkboxes over builtins +
  servers with raw-entry chips preserved).

## Testing

- `config.rs`: seeding once / persistence across reopen / delete-all stays
  deleted / old config parses / restore-defaults name matching / validator
  rules.
- `agent_tests.rs` (MockProvider, now also captures tools + system + options
  per request): typed spawn applies persona and filters the tool list
  (Explore: read-only builtins, no write, no subagent); model/effort
  overrides reach the request and the meta chips; unknown type errors before
  approval; a restricted subagent's own spawn attempt is rejected.
- `src/subagents.test.mjs`: header fallback `name → agent → "Subagent"`.

## Addendum (2026-09-21, later): visible agent type

First live QA showed the resolved type was invisible: the card header showed
the model-invented instance name (`dir-scout`) and nothing mapped it back to
the Settings list. Decisions:

- **Omitted `agent` resolves to General-Purpose** (by name, case-insensitive,
  via `builtin::DEFAULT_SUBAGENT_NAME`) — matching Claude Code/ZCode, where
  omitting the type means the general-purpose agent. Every spawn now
  corresponds to a Settings entry; editing General-Purpose also affects
  untyped spawns. If the definition was deleted, omission falls back to the
  base-generic preamble (spec `None`).
- **The type is shown on the card.** `SubagentMeta` gained `agent:
  Option<String>` (session-only, set from the spec's name). The collapsed
  card header carries a tinted type badge between the instance name and the
  status (`dir-scout — map the layout  [Explore]  Running`), and the expanded
  meta row leads with the same chip — rendered for replayed cards too, where
  the `subagentType(meta, args)` helper falls back from live meta to the
  persisted `agent` argument and finally to the label "Generic".
