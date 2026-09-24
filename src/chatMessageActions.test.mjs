import assert from "node:assert/strict";
import test from "node:test";
import { messageActionKinds } from "./chatMessageActions.ts";

const user = (text, messageIndex) => ({ kind: "user", text, messageIndex });
const assistant = (text, streaming = false) => ({
  kind: "assistant",
  text,
  streaming,
});

test("persisted user messages expose copy and edit", () => {
  assert.deepEqual(messageActionKinds(user("fix the bug", 1)), ["copy", "edit"]);
});

test("user messages without a raw index expose copy only", () => {
  assert.deepEqual(messageActionKinds(user("still sending")), ["copy"]);
});

test("special or empty user messages expose no actions", () => {
  assert.deepEqual(messageActionKinds(user("[Conversation compacted]\nsummary")), []);
  assert.deepEqual(messageActionKinds(user("[/init]\nsetup")), []);
  assert.deepEqual(messageActionKinds(user("")), []);
});

test("only finished assistant text exposes copy", () => {
  assert.deepEqual(messageActionKinds(assistant("done")), ["copy"]);
  assert.deepEqual(messageActionKinds(assistant("partial", true)), []);
  assert.deepEqual(messageActionKinds(assistant("")), []);
  assert.deepEqual(messageActionKinds({ kind: "tool", text: "result" }), []);
});
