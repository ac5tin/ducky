# ADR-0005: Group membership and order live inside the group record

- **Status:** Accepted
- **Date:** 2026-09-22

## Context

The sidebar groups chats (see
`docs/superpowers/specs/2026-09-22-chat-groups-design.md`). Two things must
persist per chat: which group it is in, and where it sits in that group. The
group list itself needs an order too.

Ducky persists all state in one versioned JSON file (`AppConfig` in
`src-tauri/src/config.rs`). There is no database and no per-record table.

Three shapes were considered:

1. `group_id` on `ConversationMeta`, order by `updated_at`.
2. `group_id` on `ConversationMeta`, plus a separate order table or index list.
3. An ordered `conversation_ids: Vec<String>` inside the group record.

Option 1 costs the least but makes a drop position meaningless: any message
bumps `updated_at`, so a within-group order would not survive the next turn.
Option 2 stores one fact in two places, which can drift.

## Decision

A group owns its members: `ChatGroup { id, title, collapsed, color,
conversation_ids }`, and `AppConfig.groups: Vec<ChatGroup>`. The `groups` array
order is the group order; the `conversation_ids` order is the within-group
order. A chat is grouped exactly when some group lists its id.

The drag layer therefore commits one order-only payload
(`group_apply_layout`), never a full group record. The command keeps title,
colour and collapsed from the server record, drops unknown conversation ids and
duplicates, and appends any group the payload omits. A malformed or stale
payload can reorder chats but cannot erase a group's name, colour or members.

## Consequences

- `ConversationMeta` is unchanged, and `AppConfig` gained one field with
  `#[serde(default)]`, so older config files load unchanged.
- Deleting a chat must prune its id from every group
  (`Store::delete_conversation`). A missed id is harmless to the UI, which
  builds its view from the known conversations.
- Colour is one `#rrggbb` for both themes, unlike ZCode's seven theme-aware
  named colours.
- Ungrouped chats stay in one block below the groups, sorted by recency. ZCode
  interleaves them through a separate top-level order table; that is
  deliberately not copied.
