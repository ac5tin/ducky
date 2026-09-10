import assert from "node:assert/strict";
import test from "node:test";
import { allMcpIds, nextMcpIds } from "./chatMcpIds.ts";

test("turning one connector off from all-on writes the rest", () => {
  assert.deepEqual(nextMcpIds(null, ["a", "b", "c"], "a", false), ["b", "c"]);
});

test("turning the last missing connector on collapses to all-on", () => {
  assert.equal(nextMcpIds(["a", "b"], ["a", "b", "c"], "c", true), null);
});

test("all on is null and all off is empty", () => {
  assert.equal(allMcpIds(["a", "b"], true), null);
  assert.deepEqual(allMcpIds(["a", "b"], false), []);
});
