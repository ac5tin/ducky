import assert from "node:assert/strict";
import test from "node:test";
import {
  resolveDraftModel,
  resolveShownEffort,
  resolveShownModel,
} from "./chatDraft.ts";

const provider = {
  id: "p1",
  default_model: "provider-default",
  models: ["first", "provider-default", "other"],
};

function config(overrides = {}) {
  return {
    settings: {
      default_provider_id: "p1",
      default_model: "app-default",
      default_effort: "high",
      ...overrides,
    },
  };
}

test("a pick staged on the draft page wins over everything", () => {
  assert.equal(resolveDraftModel("other", config(), provider), "other");
});

test("a model staged for another provider is not sent to this one", () => {
  // switching providers on the draft page keeps no model from the last one
  assert.equal(resolveDraftModel("claude-opus-5", config(), provider), "app-default");
  assert.equal(
    resolveDraftModel("claude-opus-5", null, { ...provider, default_model: null }),
    "first",
  );
});

test("the app default model beats the provider's own default", () => {
  assert.equal(resolveDraftModel(null, config(), provider), "app-default");
});

test("the app default model only applies to the default provider", () => {
  assert.equal(
    resolveDraftModel(null, config({ default_provider_id: "p2" }), provider),
    "provider-default",
  );
});

test("falls back to the provider default, then its first model", () => {
  const noDefault = { ...provider, default_model: null };
  assert.equal(resolveDraftModel(null, null, noDefault), "first");
  assert.equal(resolveDraftModel(null, null, { ...noDefault, models: [] }), "");
});

test("shown model prefers the conversation's own, ignoring staged picks", () => {
  const conversation = { model: "in-chat" };
  assert.equal(
    resolveShownModel(conversation, "c1", "staged", config(), provider),
    "in-chat",
  );
});

test("shown model on the draft page falls through to creation resolution", () => {
  assert.equal(
    resolveShownModel(undefined, null, null, config(), provider),
    "app-default",
  );
  assert.equal(
    resolveShownModel(undefined, null, "other", config(), provider),
    "other",
  );
  assert.equal(resolveShownModel(undefined, null, null, config(), null), "");
});

test("shown effort: conversation's own mid-chat, staged pick on draft", () => {
  const conversation = { effort: "low" };
  assert.equal(
    resolveShownEffort("c1", conversation, "high", config()),
    "low",
  );
  assert.equal(
    resolveShownEffort(null, undefined, "low", config()),
    "low",
  );
});

test("untouched draft previews the app default effort creation seeds", () => {
  assert.equal(resolveShownEffort(null, undefined, undefined, config()), "high");
  // explicit "Default" beats the app default and is staged as null
  assert.equal(resolveShownEffort(null, undefined, null, config()), null);
  assert.equal(
    resolveShownEffort(null, undefined, undefined, config({ default_effort: null })),
    null,
  );
});
