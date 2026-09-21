import assert from "node:assert/strict";
import test from "node:test";
import {
  activityTool,
  applySubagentDeltas,
  subagentActivityLabel,
  subagentHeader,
  subagentTask,
  subagentType,
} from "./subagents.ts";

const toolItem = (id, state = {}) => ({
  kind: "tool",
  id,
  state: { tool_call_id: id, status: "running", ...state },
});

test("applySubagentDeltas appends onto matching tool items", () => {
  const items = [
    { kind: "user", id: "u", text: "hi" },
    toolItem("call-1"),
    toolItem("call-2", { subagent_text: "start" }),
  ];
  const next = applySubagentDeltas(items, { "call-1": "hello", "call-2": "…" });
  assert.equal(next[1].state.subagent_text, "hello");
  assert.equal(next[2].state.subagent_text, "start…");
  // non-tool items untouched, new array only when something applied
  assert.equal(next[0], items[0]);
  assert.notEqual(next, items);
});

test("applySubagentDeltas is a no-op without matches", () => {
  const items = [toolItem("call-1")];
  assert.equal(applySubagentDeltas(items, {}), items);
  assert.equal(applySubagentDeltas(items, { other: "x" }), items);
});

test("subagentHeader prefers name + description, falls back to task", () => {
  assert.deepEqual(
    subagentHeader({
      name: "repo-explorer",
      description: "find all callers of run_turn",
      task: "long task text\nsecond line",
    }),
    { name: "repo-explorer", brief: "find all callers of run_turn" },
  );
  // brief falls back to the task's first line
  assert.deepEqual(subagentHeader({ task: "  first line\nsecond" }), {
    name: "Subagent",
    brief: "first line",
  });
  // nothing usable at all
  assert.deepEqual(subagentHeader({}), { name: "Subagent", brief: "" });
  assert.deepEqual(subagentHeader(undefined), { name: "Subagent", brief: "" });
});

test("subagentHeader name falls back to the agent type before Subagent", () => {
  // explicit name still wins
  assert.deepEqual(
    subagentHeader({ agent: "Explore", name: "scout", task: "t" }),
    { name: "scout", brief: "t" },
  );
  // no name: the configured agent type names the card
  assert.deepEqual(subagentHeader({ agent: "Explore", task: "find things" }), {
    name: "Explore",
    brief: "find things",
  });
  // blank agent is ignored
  assert.deepEqual(subagentHeader({ agent: "  ", task: "t" }), {
    name: "Subagent",
    brief: "t",
  });
});

test("subagentHeader caps the brief at 80 chars", () => {
  const long = "y".repeat(120);
  const { brief } = subagentHeader({ description: long });
  assert.equal(brief.length, 80);
  assert.ok(brief.endsWith("…"));
});

test("subagentTask returns the full task text", () => {
  assert.equal(subagentTask({ task: "do\nit" }), "do\nit");
  assert.equal(subagentTask({ task: "" }), "");
  assert.equal(subagentTask({}), "");
  assert.equal(subagentTask(undefined), "");
});

test("subagentActivityLabel names the tool and status", () => {
  assert.equal(subagentActivityLabel("ducky__fs_read", "running"), "ducky__fs_read · running");
  assert.equal(subagentActivityLabel(undefined, "done"), "tool · done");
});

test("activityTool recovers the tool name from a previous line", () => {
  assert.equal(activityTool("ducky__fs_list · running"), "ducky__fs_list");
  // the fallback placeholder is not a real name
  assert.equal(activityTool("tool · done"), undefined);
  assert.equal(activityTool(undefined), undefined);
  assert.equal(activityTool(""), undefined);
  // round trip: a label built without a tool stays nameless
  const label = subagentActivityLabel(undefined, "done");
  assert.equal(activityTool(label), undefined);
});

test("subagentType prefers live meta, falls back to args, then Generic", () => {
  // live meta is authoritative — including a present-but-null agent
  assert.equal(subagentType({ agent: "Explore" }, { agent: "ignored" }), "Explore");
  assert.equal(subagentType({ agent: null }, { agent: "Explore" }), "Generic");
  assert.equal(subagentType({}, { agent: "Explore" }), "Generic");
  // no meta (replay / pre-approval): the raw argument decides
  assert.equal(subagentType(undefined, { agent: "Explore" }), "Explore");
  assert.equal(subagentType(null, { agent: "  " }), "Generic");
  assert.equal(subagentType(undefined, {}), "Generic");
  assert.equal(subagentType(undefined, undefined), "Generic");
});
