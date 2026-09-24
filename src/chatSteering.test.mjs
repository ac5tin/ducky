import test from "node:test";
import assert from "node:assert/strict";

import {
  nextFlushableSteer,
  withSteeringAdded,
  withSteeringRemoved,
} from "./chatSteering.ts";

const msg = (id, text) => ({ id, text, ts: "2026-09-24T00:00:00Z" });

test("adds a steering message without touching other conversations", () => {
  const before = { a: [msg("1", "hello")] };
  const after = withSteeringAdded(before, "b", msg("2", "steer"));
  assert.deepEqual(after.a, [msg("1", "hello")]);
  assert.deepEqual(after.b, [msg("2", "steer")]);
  assert.deepEqual(before, { a: [msg("1", "hello")] }, "input is not mutated");
});

test("removes by id, not by text", () => {
  const before = { a: [msg("1", "same"), msg("2", "same")] };
  const after = withSteeringRemoved(before, "a", "1");
  assert.deepEqual(after.a, [msg("2", "same")]);
});

test("removing an unknown id changes nothing", () => {
  const before = { a: [msg("1", "x")] };
  assert.deepEqual(withSteeringRemoved(before, "a", "nope").a, before.a);
});

test("a delivery for a background conversation still clears its queue", () => {
  const before = { a: [msg("1", "x")], b: [msg("2", "y")] };
  const after = withSteeringRemoved(before, "b", "2");
  assert.deepEqual(after.b, []);
  assert.deepEqual(after.a, [msg("1", "x")]);
});

test("flush skips a steer whose invoke is still in flight", () => {
  const queue = [msg("1", "in flight"), msg("2", "settled")];
  assert.deepEqual(nextFlushableSteer(queue, new Set(["1"])), msg("2", "settled"));
});

test("flush waits when every pending steer is in flight", () => {
  const queue = [msg("1", "a"), msg("2", "b")];
  assert.equal(nextFlushableSteer(queue, new Set(["1", "2"])), undefined);
});

test("flush takes the first steer when nothing is in flight", () => {
  const queue = [msg("1", "a"), msg("2", "b")];
  assert.deepEqual(nextFlushableSteer(queue, new Set()), msg("1", "a"));
});
