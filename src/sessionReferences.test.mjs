import test from "node:test";
import assert from "node:assert/strict";

import {
  sessionReferenceInsertion,
  sessionReferenceMatches,
  sessionReferenceToken,
} from "./sessionReferences.ts";

const chat = (id, title, updated_at) => ({
  id,
  title,
  updated_at,
  created_at: updated_at,
  provider_id: "p",
  model: "m",
  effort: null,
  mode: "default",
});

test("sessionReferenceToken finds a # token at a word boundary", () => {
  assert.deepEqual(sessionReferenceToken("#", 1), { start: 0, query: "" });
  assert.deepEqual(sessionReferenceToken("#holi", 5), { start: 0, query: "holi" });
  assert.deepEqual(sessionReferenceToken("from #holi", 10), {
    start: 5,
    query: "holi",
  });
});

test("sessionReferenceToken ignores # inside a word and finished tokens", () => {
  assert.equal(sessionReferenceToken("C#", 2), null);
  assert.equal(sessionReferenceToken("issue#42", 8), null);
  // whitespace closes the token
  assert.equal(sessionReferenceToken("#holi day", 9), null);
  // the caret sits before the #
  assert.equal(sessionReferenceToken("#holi", 0), null);
});

test("sessionReferenceMatches excludes the active chat and filters titles", () => {
  const chats = [
    chat("a", "Holiday plans", "2026-09-01T00:00:00Z"),
    chat("b", "Weekend trip", "2026-09-03T00:00:00Z"),
    chat("c", "Refactor notes", "2026-09-02T00:00:00Z"),
  ];
  assert.deepEqual(
    sessionReferenceMatches(chats, "", null).map((c) => c.id),
    ["b", "c", "a"],
  );
  assert.deepEqual(
    sessionReferenceMatches(chats, "trip", null).map((c) => c.id),
    ["b"],
  );
  assert.deepEqual(
    sessionReferenceMatches(chats, "", "b").map((c) => c.id),
    ["c", "a"],
  );
});

test("sessionReferenceMatches caps the list at 20, newest first", () => {
  const many = Array.from({ length: 25 }, (_, i) =>
    chat(
      `id-${i}`,
      `chat ${i}`,
      `2026-09-${String(i + 1).padStart(2, "0")}T00:00:00Z`,
    ),
  );
  const out = sessionReferenceMatches(many, "", null);
  assert.equal(out.length, 20);
  assert.equal(out[0].id, "id-24");
});

test("sessionReferenceInsertion writes the id-backed token", () => {
  assert.equal(sessionReferenceInsertion("abc-123"), "#chat_abc-123 ");
});
