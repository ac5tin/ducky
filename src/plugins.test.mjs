import test from "node:test";
import assert from "node:assert/strict";

import {
  availableUpdates,
  filterCatalog,
  formatVersion,
  groupByCategory,
  hoursToInterval,
  pluginStatusLabel,
  runInstallPlugin,
  shouldSchedulePluginMaintenance,
  skillCount,
  withPluginProgress,
  withUpdateAvailable,
  withoutPluginProgress,
} from "./plugins.ts";

// Fixtures mirror the backend JSON (`CatalogEntry` in
// `src-tauri/src/plugins/manager.rs`); field names are snake_case as serialized.
const entry = (over = {}) => ({
  marketplace_id: "official",
  name: "demo",
  display_name: null,
  description: null,
  version: "1.0.0",
  category: null,
  tags: [],
  author: null,
  homepage: null,
  icon: null,
  keywords: [],
  available: true,
  reason: null,
  source_kind: "github",
  installed: null,
  installed_version: null,
  update_available: false,
  ...over,
});

const plugin = (over = {}) => ({ id: "demo", name: "Demo", ...over });

test("filterCatalog matches name, description and keywords", () => {
  const entries = [
    entry({ name: "acme-tools", description: "Useless helpers" }),
    entry({ name: "other", description: "Finds widgets" }),
    entry({ name: "third", keywords: ["gadget", "sprocket"] }),
  ];
  assert.deepEqual(
    filterCatalog(entries, "acme", null).map((e) => e.name),
    ["acme-tools"],
  );
  assert.deepEqual(
    filterCatalog(entries, "widgets", null).map((e) => e.name),
    ["other"],
  );
  assert.deepEqual(
    filterCatalog(entries, "sprocket", null).map((e) => e.name),
    ["third"],
  );
  // the display name is searchable too
  assert.deepEqual(
    filterCatalog([entry({ display_name: "Nice Display" })], "nice", null).length,
    1,
  );
});

test("filterCatalog is case-insensitive and trims", () => {
  const entries = [entry({ name: "AcMe-Tools" })];
  assert.equal(filterCatalog(entries, "  aCmE  ", null).length, 1);
});

test("filterCatalog returns everything for an empty query", () => {
  const entries = [entry({ name: "a" }), entry({ name: "b" })];
  assert.equal(filterCatalog(entries, "", null).length, 2);
  assert.equal(filterCatalog(entries, "   ", null).length, 2);
  assert.equal(filterCatalog(entries, undefined, null).length, 2);
  // an empty query still applies the category filter
  assert.equal(
    filterCatalog([...entries, entry({ name: "c", category: "tools" })], "", "tools")
      .map((e) => e.name)
      .join(","),
    "c",
  );
});

test("groupByCategory puts uncategorised entries last", () => {
  const entries = [
    entry({ name: "b", category: "tools" }),
    entry({ name: "none", category: null }),
    entry({ name: "a", category: "agents" }),
    entry({ name: "b2", category: "tools" }),
  ];
  const groups = groupByCategory(entries);
  assert.deepEqual(
    groups.map((g) => g.category),
    ["agents", "tools", ""],
  );
  assert.deepEqual(
    groups[1].entries.map((e) => e.name),
    ["b", "b2"],
  );
  assert.deepEqual(
    groups[2].entries.map((e) => e.name),
    ["none"],
  );
});

test("availableUpdates returns only plugins with availableUpdate", () => {
  const plugins = [
    plugin({ id: "a", available_update: { version: "2.0.0", resolvedSha: null } }),
    plugin({ id: "b", available_update: null }),
    plugin({ id: "c" }),
  ];
  assert.deepEqual(
    availableUpdates(plugins).map((p) => p.id),
    ["a"],
  );
});

test("formatVersion renders 'unknown' for missing versions", () => {
  assert.equal(formatVersion(undefined), "unknown");
  assert.equal(formatVersion(null), "unknown");
  assert.equal(formatVersion(""), "unknown");
  assert.equal(formatVersion("   "), "unknown");
  assert.equal(formatVersion("1.2.3"), "1.2.3");
});

test("pluginStatusLabel prefers error over update_available", () => {
  const failing = plugin({
    status: "update_available",
    available_update: { version: "2.0.0", resolvedSha: null },
    diagnostics: [{ level: "error", target: "manifest", message: "bad" }],
  });
  assert.equal(pluginStatusLabel(failing), "Error");

  const clean = plugin({
    status: "update_available",
    available_update: { version: "2.0.0", resolvedSha: null },
    diagnostics: [],
  });
  assert.equal(pluginStatusLabel(clean), "Update available");

  assert.equal(
    pluginStatusLabel(plugin({ status: "enabled", diagnostics: [] })),
    "Enabled",
  );
  assert.equal(
    pluginStatusLabel(plugin({ status: "installed_disabled", diagnostics: [] })),
    "Disabled",
  );
  assert.equal(
    pluginStatusLabel(plugin({ status: "modified_locally", diagnostics: [] })),
    "Modified locally",
  );
});

test("skillCount counts the skills the plugin row carries", () => {
  assert.equal(skillCount(plugin({ skills: [{}, {}] })), 2);
  assert.equal(skillCount(plugin({})), 0);
});

test("hoursToInterval maps 0 hours to no timer", () => {
  assert.equal(hoursToInterval(0), null);
  assert.equal(hoursToInterval(-1), null);
  assert.equal(hoursToInterval(6), 21_600_000);
});

test("shouldSchedulePluginMaintenance never schedules in dev", () => {
  // the dev guard: a positive interval is still a no in a dev build
  assert.equal(shouldSchedulePluginMaintenance({ dev: true, hours: 6 }), false);
  assert.equal(shouldSchedulePluginMaintenance({ dev: false, hours: 6 }), true);
  // 0 / negative hours mean "startup only", in every build
  assert.equal(shouldSchedulePluginMaintenance({ dev: false, hours: 0 }), false);
  assert.equal(shouldSchedulePluginMaintenance({ dev: false, hours: -1 }), false);
  assert.equal(shouldSchedulePluginMaintenance({ dev: true, hours: 0 }), false);
});

test("plugin progress is keyed by the install record id", () => {
  const first = withPluginProgress(
    {},
    { plugin_id: "acme", phase: "fetch", detail: "fetching the plugin source" },
  );
  assert.deepEqual(first, {
    acme: { phase: "fetch", detail: "fetching the plugin source" },
  });
  const second = withPluginProgress(first, {
    plugin_id: "other",
    phase: "place",
    detail: "placed",
  });
  assert.equal(second.acme.phase, "fetch");
  assert.equal(second.other.detail, "placed");
  // a later phase for the same id replaces the row, never appends
  const third = withPluginProgress(second, {
    plugin_id: "acme",
    phase: "validate",
    detail: "validated",
  });
  assert.equal(third.acme.phase, "validate");
  assert.equal(Object.keys(third).length, 2);
});

test("withUpdateAvailable patches only the named row", () => {
  const plugins = [
    plugin({ id: "a", status: "enabled" }),
    plugin({ id: "b", status: "enabled" }),
  ];
  const next = withUpdateAvailable(plugins, {
    plugin_id: "b",
    from: "1.0.0",
    to: "2.0.0",
  });
  assert.equal(next[0], plugins[0]);
  assert.equal(next[1].status, "update_available");
  assert.deepEqual(next[1].available_update, {
    version: "2.0.0",
    resolvedSha: null,
  });
  // a status that outranks an update is kept, matching pluginStatusLabel
  assert.equal(
    withUpdateAvailable([plugin({ id: "c", status: "modified_locally" })], {
      plugin_id: "c",
      from: null,
      to: "3.0.0",
    })[0].status,
    "modified_locally",
  );
  // an unknown id is a no-op, and the array identity is kept
  assert.equal(
    withUpdateAvailable(plugins, { plugin_id: "zz", from: null, to: null }),
    plugins,
  );
});

test("withoutPluginProgress drops only the named row", () => {
  const state = {
    a: { phase: "place", detail: "placed" },
    b: { phase: "fetch", detail: "fetching" },
  };
  assert.deepEqual(withoutPluginProgress(state, "a"), { b: state.b });
  assert.equal(withoutPluginProgress(state, "zz"), state);
});

test("runInstallPlugin sets catalogLoading around the install", async () => {
  const seen = [];
  const detail = await runInstallPlugin(
    {
      pluginInstall: async (req) => {
        seen.push("install");
        return { id: "acme", name: "Acme", request: req };
      },
    },
    { marketplace: "official", name: "acme" },
    (loading) => seen.push(loading),
  );
  assert.deepEqual(seen, [true, "install", false]);
  assert.equal(detail.id, "acme");
});

test("runInstallPlugin clears catalogLoading when the install rejects", async () => {
  const seen = [];
  await assert.rejects(
    runInstallPlugin(
      {
        pluginInstall: async () => {
          throw new Error("unsafe id");
        },
      },
      { source: { kind: "path", path: "/tmp/x" } },
      (loading) => seen.push(loading),
    ),
    /unsafe id/,
  );
  assert.deepEqual(seen, [true, false]);
});
