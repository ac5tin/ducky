import assert from "node:assert/strict";
import test from "node:test";
import { advisorChipState, supportedEffort } from "./advisorChip.ts";

const providers = [{ id: "p1", kind: "custom", name: "One" }];
const settings = {
  advisor_enabled_by_default: false,
  advisor_provider_id: "p1",
  advisor_model: "m2",
  advisor_effort: "high",
};
const meta = {
  advisor_enabled: true,
  advisor_provider_id: null,
  advisor_model: null,
  advisor_effort: null,
};

test("off when the chat has the advisor disabled", () => {
  const state = advisorChipState({ ...meta, advisor_enabled: false }, settings, providers);
  assert.equal(state.mode, "off");
});

test("on via the settings default", () => {
  const state = advisorChipState(meta, settings, providers);
  assert.equal(state.mode, "on");
  assert.equal(state.model, "m2");
  assert.equal(state.effort, "high");
});

test("session override wins", () => {
  const state = advisorChipState(
    { ...meta, advisor_provider_id: "p1", advisor_model: "m9", advisor_effort: "low" },
    settings,
    providers,
  );
  assert.equal(state.mode, "on");
  assert.equal(state.model, "m9");
  assert.equal(state.effort, "low");
});

test("unresolved when the provider is gone", () => {
  const state = advisorChipState(
    { ...meta, advisor_provider_id: "gone" },
    settings,
    providers,
  );
  assert.equal(state.mode, "unresolved");
});

// the popover writes the returned tuple back verbatim, so the raw override must
// survive an unresolvable provider instead of being silently cleared
test("an unresolvable override keeps its raw provider, model and effort", () => {
  const state = advisorChipState(
    { ...meta, advisor_provider_id: "gone", advisor_model: "m9", advisor_effort: "low" },
    settings,
    providers,
  );
  assert.equal(state.mode, "unresolved");
  assert.equal(state.provider_id, "gone");
  assert.equal(state.model, "m9");
  assert.equal(state.effort, "low");
});

test("unresolved when nothing is configured", () => {
  const state = advisorChipState(meta, { ...settings, advisor_provider_id: null }, providers);
  assert.equal(state.mode, "unresolved");
});

test("the draft state follows the settings default", () => {
  const state = advisorChipState(null, { ...settings, advisor_enabled_by_default: true }, providers);
  assert.equal(state.mode, "on");
});

test("supported effort stays when the new model lists it", () => {
  assert.equal(supportedEffort("high", ["low", "high"]), "high");
});

test("supported effort clears when the new model does not list it", () => {
  assert.equal(supportedEffort("max", ["low", "high"]), null);
});

test("supported effort keeps null", () => {
  assert.equal(supportedEffort(null, ["low", "high"]), null);
});

test("an empty level list clears a stored effort", () => {
  assert.equal(supportedEffort("high", []), null);
});
