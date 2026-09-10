import assert from "node:assert/strict";
import test from "node:test";
import { initialToolDetailsOpen } from "./toolDetails.ts";

test("auto closes a finished call", () => {
  assert.equal(initialToolDetailsOpen("auto", "done"), false);
});

test("auto opens a running call", () => {
  assert.equal(initialToolDetailsOpen("auto", "running"), true);
});

test("collapsed stays closed", () => {
  assert.equal(initialToolDetailsOpen("collapsed", "running"), false);
  assert.equal(initialToolDetailsOpen("collapsed", "done"), false);
});

test("expanded stays open", () => {
  assert.equal(initialToolDetailsOpen("expanded", "running"), true);
  assert.equal(initialToolDetailsOpen("expanded", "done"), true);
});
