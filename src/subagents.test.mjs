import assert from "node:assert/strict";
import test from "node:test";
import {
  applySubagentDeltas,
  subagentActivityLabel,
  subagentTaskSnippet,
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

test("subagentTaskSnippet takes the first line, capped at 80 chars", () => {
  assert.equal(subagentTaskSnippet({ task: "Do a thing" }), "Do a thing");
  assert.equal(
    subagentTaskSnippet({ task: "  first line\nsecond line" }),
    "first line",
  );
  const long = "x".repeat(120);
  assert.equal(subagentTaskSnippet({ task: long }).length, 80);
  assert.ok(subagentTaskSnippet({ task: long }).endsWith("…"));
  assert.equal(subagentTaskSnippet({}), "");
  assert.equal(subagentTaskSnippet({ task: "   " }), "");
  assert.equal(subagentTaskSnippet(undefined), "");
});

test("subagentActivityLabel names the tool and status", () => {
  assert.equal(subagentActivityLabel("ducky__fs_read", "running"), "ducky__fs_read · running");
  assert.equal(subagentActivityLabel(undefined, "done"), "tool · done");
});
