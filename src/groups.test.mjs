import test from "node:test";
import assert from "node:assert/strict";

import {
  GROUP_COLORS,
  buildSidebarView,
  chatDragId,
  chatZone,
  computeDropLayout,
  groupDragId,
  groupEmptyZone,
  groupFooterZone,
  groupHeaderZone,
  groupOverZone,
  layoutSignature,
  normalizeGroupColor,
  parseDragId,
  parseZone,
  toLayout,
} from "./groups.ts";

const chat = (id, updated_at = "2026-09-22T10:00:00Z") => ({
  id,
  title: id,
  provider_id: "p1",
  model: "m",
  effort: null,
  mode: "default",
  created_at: updated_at,
  updated_at,
});

const group = (id, conversation_ids = [], extra = {}) => ({
  id,
  title: id,
  collapsed: false,
  color: GROUP_COLORS[0],
  conversation_ids,
  ...extra,
});

/** `["g1:[a,b]", "g2:[c]"]` — compact layout assertions. */
const shape = (groups) =>
  groups.map((g) => `${g.id}:[${g.conversation_ids.join(",")}]`);

const view = (groups, chats) => buildSidebarView(chats, groups);

test("buildSidebarView keeps group order, drops unknown ids, sorts the ungrouped", () => {
  const built = buildSidebarView(
    [chat("a", "2026-09-20T00:00:00Z"), chat("b", "2026-09-22T00:00:00Z"), chat("c", "2026-09-21T00:00:00Z")],
    [group("g1", ["c", "gone", "a"]), group("g2", [])],
  );
  assert.deepEqual(
    built.groups.map((n) => n.chats.map((c) => c.id)),
    [["c", "a"], []],
  );
  assert.deepEqual(built.ungrouped.map((c) => c.id), ["b"]);
});

test("buildSidebarView never shows the same chat twice", () => {
  const built = buildSidebarView([chat("a")], [group("g1", ["a"]), group("g2", ["a"])]);
  assert.deepEqual(
    built.groups.map((n) => n.chats.map((c) => c.id)),
    [["a"], []],
  );
});

test("normalizeGroupColor accepts hex and falls back to the gray preset", () => {
  assert.equal(normalizeGroupColor("#ABC"), "#aabbcc");
  assert.equal(normalizeGroupColor("#0EA5E9"), "#0ea5e9");
  assert.equal(normalizeGroupColor("  #0ea5e9  "), "#0ea5e9");
  for (const bad of ["red", "#12345", "#gggggg", "", null, undefined]) {
    assert.equal(normalizeGroupColor(bad), GROUP_COLORS[0], `expected gray for ${bad}`);
  }
});

test("toLayout strips everything but order and membership", () => {
  const layout = toLayout([
    group("g1", ["a"], { title: "Work", color: "#ef4444", collapsed: true }),
  ]);
  assert.deepEqual(layout, [{ id: "g1", conversation_ids: ["a"] }]);
});

test("parseZone understands every droppable id and rejects the rest", () => {
  assert.deepEqual(parseZone(chatZone("a")), { kind: "chat", conversationId: "a" });
  assert.deepEqual(parseZone(groupHeaderZone("g1")), { kind: "group-header", groupId: "g1" });
  assert.deepEqual(parseZone(groupFooterZone("g1")), { kind: "group-footer", groupId: "g1" });
  assert.deepEqual(parseZone(groupEmptyZone("g1")), { kind: "group-empty", groupId: "g1" });
  assert.deepEqual(parseZone(groupOverZone("g1")), { kind: "group-over", groupId: "g1" });
  assert.equal(parseZone("drag-chat:a"), null);
  assert.equal(parseZone("nonsense"), null);
});

test("parseDragId returns the dragged item, or null", () => {
  assert.deepEqual(parseDragId(chatDragId("a")), { kind: "chat", id: "a" });
  assert.deepEqual(parseDragId(groupDragId("g1")), { kind: "group", id: "g1" });
  assert.equal(parseDragId(chatZone("a")), null);
});

test("a chat dropped on a chat joins that chat's group at that position", () => {
  const origin = [group("g1", ["a", "b"]), group("g2", ["c"])];
  const built = view(origin, [chat("a"), chat("b"), chat("c"), chat("d")]);
  const next = computeDropLayout(
    built,
    { kind: "chat", id: "d" },
    { kind: "chat", conversationId: "b" },
    "before",
  );
  assert.deepEqual(shape(next), ["g1:[a,d,b]", "g2:[c]"]);
});

test("a chat dropped on an ungrouped chat leaves its group", () => {
  const origin = [group("g1", ["a", "b"])];
  const built = view(origin, [chat("a"), chat("b"), chat("d")]);
  const next = computeDropLayout(
    built,
    { kind: "chat", id: "a" },
    { kind: "chat", conversationId: "d" },
    "after",
  );
  assert.deepEqual(shape(next), ["g1:[b]"]);
});

test("a chat dropped on itself changes nothing", () => {
  const origin = [group("g1", ["a", "b"]), group("g2", ["c"])];
  const built = view(origin, [chat("a"), chat("b"), chat("c")]);
  const next = computeDropLayout(
    built,
    { kind: "chat", id: "a" },
    { kind: "chat", conversationId: "a" },
    "after",
  );
  assert.equal(layoutSignature(next), layoutSignature(origin));
});

test("a chat dropped past its own neighbour keeps the requested position", () => {
  const origin = [group("g1", ["a", "b", "c"])];
  const built = view(origin, [chat("a"), chat("b"), chat("c")]);

  const before = computeDropLayout(
    built,
    { kind: "chat", id: "a" },
    { kind: "chat", conversationId: "c" },
    "before",
  );
  assert.deepEqual(shape(before), ["g1:[b,a,c]"]);

  const after = computeDropLayout(
    built,
    { kind: "chat", id: "a" },
    { kind: "chat", conversationId: "c" },
    "after",
  );
  assert.deepEqual(shape(after), ["g1:[b,c,a]"]);
});

test("a chat dropped on an expanded group header goes inside, or out", () => {
  const origin = [group("g1", ["a", "b"]), group("g2", ["c"])];
  const chats = [chat("a"), chat("b"), chat("c"), chat("d")];
  const target = { kind: "group-header", groupId: "g1" };

  const down = computeDropLayout(view(origin, chats), { kind: "chat", id: "d" }, target, "after");
  assert.deepEqual(shape(down), ["g1:[d,a,b]", "g2:[c]"]);

  const up = computeDropLayout(view(origin, chats), { kind: "chat", id: "c" }, target, "before");
  assert.deepEqual(shape(up), ["g1:[a,b]", "g2:[]"]);
});

test("a chat dropped on a collapsed group header never goes inside", () => {
  const origin = [group("g1", ["a"], { collapsed: true }), group("g2", ["c"])];
  const built = view(origin, [chat("a"), chat("c")]);
  const next = computeDropLayout(
    built,
    { kind: "chat", id: "c" },
    { kind: "group-header", groupId: "g1" },
    "after",
  );
  assert.deepEqual(shape(next), ["g1:[a]", "g2:[]"]);
});

test("a chat dropped on a group footer goes to the end, or out", () => {
  const origin = [group("g1", ["a", "b"]), group("g2", ["c"])];
  const chats = [chat("a"), chat("b"), chat("c")];
  const target = { kind: "group-footer", groupId: "g1" };

  const intoEnd = computeDropLayout(view(origin, chats), { kind: "chat", id: "c" }, target, "before");
  assert.deepEqual(shape(intoEnd), ["g1:[a,b,c]", "g2:[]"]);

  const out = computeDropLayout(view(origin, chats), { kind: "chat", id: "a" }, target, "after");
  assert.deepEqual(shape(out), ["g1:[b]", "g2:[c]"]);
});

test("a chat dropped on an empty group zone joins that group", () => {
  const origin = [group("g1", []), group("g2", ["c"])];
  const built = view(origin, [chat("c")]);
  const next = computeDropLayout(
    built,
    { kind: "chat", id: "c" },
    { kind: "group-empty", groupId: "g1" },
    "after",
  );
  assert.deepEqual(shape(next), ["g1:[c]", "g2:[]"]);
});

test("a group dropped on another group takes its position", () => {
  const origin = [group("g1", ["a"]), group("g2", ["b"]), group("g3", ["c"])];
  const built = view(origin, [chat("a"), chat("b"), chat("c")]);
  const next = computeDropLayout(
    built,
    { kind: "group", id: "g3" },
    { kind: "group-over", groupId: "g1" },
    "before",
  );
  assert.deepEqual(shape(next), ["g3:[c]", "g1:[a]", "g2:[b]"]);
});

test("a group dropped on a chat of its own group changes nothing", () => {
  const origin = [group("g1", ["a"]), group("g2", ["b"])];
  const built = view(origin, [chat("a"), chat("b")]);
  const next = computeDropLayout(
    built,
    { kind: "group", id: "g1" },
    { kind: "chat", conversationId: "a" },
    "after",
  );
  assert.equal(layoutSignature(next), layoutSignature(origin));
});

test("a group dropped on an ungrouped chat goes to the end of the group list", () => {
  const origin = [group("g1", ["a"]), group("g2", ["b"])];
  const built = view(origin, [chat("a"), chat("b"), chat("d")]);
  const next = computeDropLayout(
    built,
    { kind: "group", id: "g1" },
    { kind: "chat", conversationId: "d" },
    "after",
  );
  assert.deepEqual(shape(next), ["g2:[b]", "g1:[a]"]);
});