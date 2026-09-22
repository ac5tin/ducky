import test from "node:test";
import assert from "node:assert/strict";

import { MODE_META, MODE_ORDER, nextMode, resolveShownMode } from "./modes.ts";

test("nextMode cycles through every mode and wraps", () => {
  assert.equal(nextMode("default"), "readonly");
  assert.equal(nextMode("readonly"), "plan");
  assert.equal(nextMode("plan"), "auto");
  assert.equal(nextMode("auto"), "default");
});

test("every mode has a label, a description and an icon", () => {
  assert.equal(Object.keys(MODE_META).length, MODE_ORDER.length);
  for (const mode of MODE_ORDER) {
    const meta = MODE_META[mode];
    assert.ok(meta, `missing meta for ${mode}`);
    assert.ok(meta.label.length > 0, `${mode} label`);
    assert.ok(meta.description.length > 0, `${mode} description`);
    assert.ok(meta.icon.length > 0, `${mode} icon`);
  }
});

test("read-only copy never promises what a server can lie about", () => {
  // a server's readOnlyHint is trusted as given (ADR-0004), so the copy must
  // not claim that nothing can change
  for (const mode of ["readonly", "plan"]) {
    assert.doesNotMatch(
      MODE_META[mode].description,
      /cannot change anything|nothing can change/i,
      `${mode} over-promises`,
    );
  }
});

test("resolveShownMode prefers the conversation, then the draft, then the app default", () => {
  const config = { settings: { default_mode: "auto" } };
  assert.equal(resolveShownMode({ mode: "plan" }, null, config), "plan");
  assert.equal(resolveShownMode(undefined, "readonly", config), "readonly");
  assert.equal(resolveShownMode(undefined, null, config), "auto");
  assert.equal(resolveShownMode(undefined, null, null), "default");
});