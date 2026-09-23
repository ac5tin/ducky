import assert from "node:assert/strict";
import test from "node:test";
import {
  latestUserMessageIndex,
  messageActionKinds,
} from "./chatMessageActions.ts";

const user = (text) => ({ kind: "user", text });
const assistant = (text, streaming = false) => ({
  kind: "assistant",
  text,
  streaming,
});

test("normal user messages expose copy and edit", () => {
  assert.deepEqual(messageActionKinds(user("fix the bug")), ["copy", "edit"]);
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

test("latest persisted user index is undefined when no user turn exists", () => {
  assert.equal(
    latestUserMessageIndex([
      { kind: "assistant" },
      { kind: "tool_result" },
    ]),
    undefined,
  );
  assert.equal(
    latestUserMessageIndex([
      { kind: "user" },
      { kind: "assistant" },
      { kind: "user" },
    ]),
    2,
  );
});
