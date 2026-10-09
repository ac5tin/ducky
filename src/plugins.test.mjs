import test from "node:test";
import assert from "node:assert/strict";

import {
  autoUpdateIds,
  availableUpdates,
  catalogEntryAction,
  filterCatalog,
  formatVersion,
  groupByCategory,
  hoursToInterval,
  marketplaceRemovable,
  pluginSkillRows,
  pluginServerOwner,
  pluginStatusLabel,
  pluginStatusTone,
  pluginTrustSummary,
  progressPhaseLabel,
  progressRowViews,
  runAddMarketplaceAction,
  runInstallPlugin,
  runInstallPluginAction,
  runPluginBatchSwap,
  runPluginServerToggle,
  runPluginSwap,
  serverTrustLine,
  shouldSchedulePluginMaintenance,
  skillCount,
  sourceLabel,
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

test("install action sets catalogLoading then clears it", async () => {
  const events = [];
  const outcome = await runInstallPluginAction(
    {
      pluginInstall: async (req) => {
        events.push("install");
        return { id: "acme", name: "Acme", request: req };
      },
      pluginsList: async () => {
        events.push("plugins");
        return [plugin({ id: "acme" })];
      },
      skillsList: async () => {
        events.push("skills");
        return [];
      },
      marketplaceCatalog: async (marketplace) => {
        events.push(`catalog:${marketplace}`);
        return [entry()];
      },
    },
    { marketplace: "official", name: "acme" },
    "official",
    (loading) => events.push(loading ? "loading:on" : "loading:off"),
  );
  // loading is raised before the fetch and cleared as soon as it settles, so
  // the reload that follows is not covered by the install's loading flag
  assert.deepEqual(events, [
    "loading:on",
    "install",
    "loading:off",
    "plugins",
    "skills",
    "catalog:official",
  ]);
  assert.equal(outcome.installed.id, "acme");
  assert.deepEqual(outcome.plugins, [plugin({ id: "acme" })]);
  assert.deepEqual(outcome.catalog, [entry()]);

  // a failed install clears the flag and never reloads anything
  const failed = [];
  await assert.rejects(
    runInstallPluginAction(
      {
        pluginInstall: async () => {
          throw new Error("unsafe id");
        },
        pluginsList: async () => {
          failed.push("plugins");
          return [];
        },
        skillsList: async () => {
          failed.push("skills");
          return [];
        },
        marketplaceCatalog: async () => {
          failed.push("catalog");
          return [];
        },
      },
      { source: { kind: "path", path: "/tmp/x" } },
      null,
      (loading) => failed.push(loading ? "loading:on" : "loading:off"),
    ),
    /unsafe id/,
  );
  assert.deepEqual(failed, ["loading:on", "loading:off"]);
});

test("adding a marketplace fetches it before anything reloads", async () => {
  const events = [];
  const summary = await runAddMarketplaceAction(
    {
      marketplaceAdd: async (input, name) => {
        events.push(`add:${input.source}:${name}`);
        return { id: "skills", name: "Skills" };
      },
      marketplaceRefresh: async (id) => {
        events.push(`refresh:${id}`);
        return [];
      },
    },
    { source: "https://github.com/ac5tin/skills" },
    undefined,
  );
  assert.deepEqual(events, [
    "add:https://github.com/ac5tin/skills:undefined",
    "refresh:skills",
  ]);
  assert.equal(summary.id, "skills");
});

test("a failed marketplace fetch rejects the add", async () => {
  await assert.rejects(
    runAddMarketplaceAction(
      {
        marketplaceAdd: async () => ({ id: "skills", name: "Skills" }),
        marketplaceRefresh: async () => {
          throw new Error("ipc gone");
        },
      },
      { source: "https://github.com/ac5tin/skills" },
      undefined,
    ),
    /ipc gone/,
  );
});

test("catalogEntryAction maps install state to one button", () => {
  const unavailable = entry({
    available: false,
    reason: "unsupported source kind: npm",
  });
  assert.deepEqual(catalogEntryAction(unavailable), {
    kind: "unavailable",
    label: "Unavailable",
    disabled: true,
    hint: "unsupported source kind: npm",
  });
  // an update outranks the installed chip; busy disables an action
  assert.deepEqual(catalogEntryAction(entry({ installed: "acme", update_available: true })), {
    kind: "update",
    label: "Update",
    disabled: false,
    hint: null,
  });
  assert.equal(
    catalogEntryAction(entry({ installed: "acme", update_available: true }), true).disabled,
    true,
  );
  assert.deepEqual(catalogEntryAction(entry({ installed: "acme" })), {
    kind: "installed",
    label: "Installed",
    disabled: true,
    hint: null,
  });
  assert.deepEqual(catalogEntryAction(entry({})), {
    kind: "install",
    label: "Install",
    disabled: false,
    hint: null,
  });
  assert.equal(catalogEntryAction(entry({}), true).disabled, true);
});

test("pluginStatusTone ranks error over update", () => {
  const withError = plugin({
    status: "error",
    diagnostics: [{ level: "error", target: "package", message: "missing" }],
  });
  assert.equal(pluginStatusTone(withError), "error");
  assert.equal(
    pluginStatusTone(plugin({ status: "update_available", diagnostics: [] })),
    "warn",
  );
  assert.equal(
    pluginStatusTone(plugin({ status: "modified_locally", diagnostics: [] })),
    "warn",
  );
  assert.equal(pluginStatusTone(plugin({ status: "invalid", diagnostics: [] })), "error");
  assert.equal(pluginStatusTone(plugin({ status: "enabled", diagnostics: [] })), "ok");
  assert.equal(
    pluginStatusTone(plugin({ status: "installed_disabled", diagnostics: [] })),
    "muted",
  );
});

test("progressPhaseLabel names the three install phases", () => {
  assert.equal(progressPhaseLabel("fetch"), "Fetching");
  assert.equal(progressPhaseLabel("validate"), "Validating");
  assert.equal(progressPhaseLabel("place"), "Placing");
  assert.equal(progressPhaseLabel("swap"), "Working");
});

test("progressRowViews keeps the record id and orders the phases", () => {
  const rows = progressRowViews({
    zeta: { phase: "place", detail: "placed" },
    acme: { phase: "fetch", detail: "fetching the source" },
  });
  assert.deepEqual(
    rows.map((row) => [row.id, row.label, row.detail]),
    [
      ["acme", "Fetching", "fetching the source"],
      ["zeta", "Placing", "placed"],
    ],
  );
  assert.deepEqual(progressRowViews({}), []);
});

test("serverTrustLine shows command args or the host only", () => {
  assert.equal(
    serverTrustLine({
      transport: "stdio",
      command: "node",
      args: ["server.js", "--flag"],
      url: null,
    }),
    "stdio — node server.js --flag",
  );
  assert.equal(
    serverTrustLine({ transport: "stdio", command: "npx", args: [], url: null }),
    "stdio — npx",
  );
  assert.equal(
    serverTrustLine({
      transport: "streamable-http",
      command: null,
      args: [],
      url: "api.example.com:443",
    }),
    "streamable-http — api.example.com:443",
  );
  assert.equal(
    serverTrustLine({ transport: "sse", command: null, args: [], url: null }),
    "sse — host unknown",
  );
});

test("sourceLabel names every source form", () => {
  assert.equal(
    sourceLabel({ kind: "github", repo: "o/r", path: null, ref: null, sha: null }),
    "o/r",
  );
  assert.equal(
    sourceLabel({ kind: "github", repo: "o/r", path: "sub", ref: null, sha: null }),
    "o/r · sub",
  );
  assert.equal(
    sourceLabel({ kind: "git", url: "https://x/y.git", path: "p", ref: null, sha: null }),
    "https://x/y.git · p",
  );
  assert.equal(
    sourceLabel({ kind: "git-subdir", url: "https://x/y.git", path: "p", ref: null, sha: null }),
    "https://x/y.git · p",
  );
  assert.equal(sourceLabel({ kind: "url", url: "https://x/m.json" }), "https://x/m.json");
  assert.equal(sourceLabel({ kind: "path", path: "/tmp/m" }), "/tmp/m");
  assert.equal(
    sourceLabel({ kind: "unsupported", sourceKind: "npm", detail: "no" }),
    "npm (unsupported)",
  );
});

test("pluginSkillRows carry the spec id and the shadow mark", () => {
  const rows = pluginSkillRows(
    plugin({
      id: "acme",
      skills: [
        { name: "alpha", description: "first", path: "/p/alpha" },
        { name: "beta", description: "second", path: "/p/beta" },
      ],
    }),
    [
      {
        id: "plugin:acme:alpha",
        name: "alpha",
        description: "first",
        origin: "plugin: acme",
        shadowed: "user:alpha",
      },
    ],
  );
  assert.deepEqual(
    rows.map((row) => [row.id, row.shadowed]),
    [
      ["plugin:acme:alpha", "user:alpha"],
      ["plugin:acme:beta", null],
    ],
  );
  assert.equal("origin" in rows[0], false, "the short provenance label is gone");
});

test("marketplaceRemovable follows the backend's reserved id", () => {
  assert.equal(marketplaceRemovable({ id: "acme", bundled: false }), true);
  assert.equal(marketplaceRemovable({ id: "acme", bundled: true }), false);
  // a hand-edited record: bundled false, but the backend still refuses it
  assert.equal(marketplaceRemovable({ id: "ducky-official", bundled: false }), false);
});

test("pluginTrustSummary lists what will run", () => {
  const summary = pluginTrustSummary(
    plugin({
      servers: [
        {
          name: "local",
          id: "plugin:acme:local",
          enabled: false,
          transport: "stdio",
          command: "node",
          args: ["index.js"],
          url: null,
        },
      ],
      subagents: [{ name: "Helper", description: "d", slug: "helper" }],
    }),
    "enabling a plugin runs its code with your user account",
  );
  assert.equal(summary.warning, "enabling a plugin runs its code with your user account");
  assert.deepEqual(summary.servers, [
    { name: "local", line: "stdio — node index.js" },
  ]);
  assert.deepEqual(summary.subagents, ["Helper"]);
});

// The plugin-server switch: the consent write owns the record, the connect
// follows only a successful enable, and the disconnect follows a successful
// disable. The store action injects the real api; these drive a fake.
function toggleApi(events, toasts, over = {}) {
  return {
    setServerEnabled: async (id, server, enabled) => {
      events.push(`consent:${id}:${server}:${enabled}`);
    },
    connect: async (serverId) => {
      events.push(`connect:${serverId}`);
      return "connected";
    },
    disconnect: async (serverId) => {
      events.push(`disconnect:${serverId}`);
    },
    isLive: () => false,
    toast: (message) => toasts.push(message),
    ...over,
  };
}

test("plugin server toggle connects after a successful enable", async () => {
  const events = [];
  const toasts = [];
  const ok = await runPluginServerToggle(
    toggleApi(events, toasts),
    "acme",
    "alpha",
    true,
  );
  assert.equal(ok, true);
  assert.deepEqual(events, [
    "consent:acme:alpha:true",
    "connect:plugin:acme:alpha",
  ]);
  assert.deepEqual(toasts, []);
});

test("plugin server toggle does not connect after a rejected enable", async () => {
  const events = [];
  const toasts = [];
  const ok = await runPluginServerToggle(
    toggleApi(events, toasts, {
      setServerEnabled: async () => {
        events.push("consent");
        throw new Error("read-only store");
      },
    }),
    "acme",
    "alpha",
    true,
  );
  assert.equal(ok, false);
  assert.deepEqual(events, ["consent"]);
  assert.equal(toasts.length, 1);
  assert.match(toasts[0], /read-only store/);
});

test("plugin server toggle skips connect when the server is already live", async () => {
  const events = [];
  const toasts = [];
  const ok = await runPluginServerToggle(
    toggleApi(events, toasts, { isLive: () => true }),
    "acme",
    "alpha",
    true,
  );
  assert.equal(ok, true);
  assert.deepEqual(events, ["consent:acme:alpha:true"]);
  assert.deepEqual(toasts, []);
});

test("plugin server toggle disconnects only after a successful disable", async () => {
  const events = [];
  const toasts = [];
  const ok = await runPluginServerToggle(
    toggleApi(events, toasts),
    "acme",
    "alpha",
    false,
  );
  assert.equal(ok, true);
  // the backend disconnect is infallible and clears a not-connected id too
  assert.deepEqual(events, [
    "consent:acme:alpha:false",
    "disconnect:plugin:acme:alpha",
  ]);
  assert.deepEqual(toasts, []);
});

test("plugin server toggle does not disconnect after a rejected disable", async () => {
  const events = [];
  const toasts = [];
  const ok = await runPluginServerToggle(
    toggleApi(events, toasts, {
      setServerEnabled: async () => {
        events.push("consent");
        throw new Error("record not written");
      },
    }),
    "acme",
    "alpha",
    false,
  );
  assert.equal(ok, false);
  assert.deepEqual(events, ["consent"]);
  assert.equal(toasts.length, 1);
});

test("plugin server toggle surfaces a failed connect", async () => {
  const toasts = [];
  await runPluginServerToggle(
    toggleApi([], toasts, { connect: async () => "error" }),
    "acme",
    "alpha",
    true,
  );
  assert.equal(toasts.length, 1);
  assert.match(toasts[0], /alpha failed to connect/);

  const thrown = [];
  await runPluginServerToggle(
    toggleApi([], thrown, {
      connect: async () => {
        throw new Error("spawn refused");
      },
    }),
    "acme",
    "alpha",
    true,
  );
  assert.equal(thrown.length, 1);
  assert.match(thrown[0], /spawn refused/);
});

test("plugin server toggle surfaces a disconnect failure", async () => {
  const toasts = [];
  await runPluginServerToggle(
    toggleApi([], toasts, {
      disconnect: async () => {
        throw new Error("ipc gone");
      },
    }),
    "acme",
    "alpha",
    false,
  );
  assert.equal(toasts.length, 1);
  assert.match(toasts[0], /ipc gone/);
});

test("autoUpdateIds applies only auto plugins that are not locally modified", () => {
  const updates = [
    { plugin_id: "a" },
    { plugin_id: "b" },
    { plugin_id: "c" },
    { plugin_id: "missing" },
    { plugin_id: "d" },
  ];
  const plugins = [
    { id: "a", update_policy: "auto", status: "update_available" },
    { id: "b", update_policy: "manual", status: "update_available" },
    { id: "c", update_policy: "auto", status: "modified_locally" },
    { id: "d", update_policy: "auto", status: "update_available" },
  ];
  assert.deepEqual(autoUpdateIds(updates, plugins), ["a", "d"]);
});

// The auto pass as the store wires it: check result -> autoUpdateIds -> the
// swap that stops the plugin's servers. The bug was a server stop before the
// refusal, so these assert the server calls, not only the outcome.
async function runAutoPass(updates, plugins, live) {
  const events = [];
  for (const id of autoUpdateIds(updates, plugins)) {
    const info = updates.find((update) => update.plugin_id === id);
    await runPluginSwap(swapApi(events, []), live[id] ?? [], async () => {
      events.push("swap");
      return info;
    });
  }
  return events;
}

test("an auto plugin modified locally keeps its servers running through a check", async () => {
  const updates = [
    {
      plugin_id: "acme",
      from: "1.0.0",
      to: "1.1.0",
      enabled_servers: ["plugin:acme:alpha"],
      modified_locally: true,
    },
  ];
  const plugins = [
    { id: "acme", update_policy: "auto", status: "modified_locally" },
  ];

  const events = await runAutoPass(updates, plugins, {
    acme: ["plugin:acme:alpha"],
  });

  assert.deepEqual(events, []);
});

test("the auto skip reads the check's local-modification flag, not the status", async () => {
  // The pre-fix check overwrote `modified_locally` with `update_available`;
  // the flag comes from the package tree hash, so no check can rewrite it.
  const updates = [
    {
      plugin_id: "acme",
      from: "1.0.0",
      to: "1.1.0",
      enabled_servers: ["plugin:acme:alpha"],
      modified_locally: true,
    },
  ];
  const plugins = [
    { id: "acme", update_policy: "auto", status: "update_available" },
  ];

  const events = await runAutoPass(updates, plugins, {
    acme: ["plugin:acme:alpha"],
  });

  assert.deepEqual(events, []);
});

test("an auto plugin that is not modified is still updated", async () => {
  const updates = [
    {
      plugin_id: "acme",
      from: "1.0.0",
      to: "1.1.0",
      enabled_servers: ["plugin:acme:alpha"],
      modified_locally: false,
    },
  ];
  const plugins = [
    { id: "acme", update_policy: "auto", status: "update_available" },
  ];

  const events = await runAutoPass(updates, plugins, {
    acme: ["plugin:acme:alpha"],
  });

  assert.deepEqual(events, [
    "disconnect:plugin:acme:alpha",
    "swap",
    "connect:plugin:acme:alpha",
  ]);
});

function swapApi(events, toasts) {
  return {
    disconnect: async (serverId) => {
      events.push(`disconnect:${serverId}`);
    },
    connect: async (serverId) => {
      events.push(`connect:${serverId}`);
      return "connected";
    },
    toast: (message) => toasts.push(message),
  };
}

test("plugin swap stops live servers before the swap and restarts them after", async () => {
  const events = [];
  const toasts = [];
  const info = await runPluginSwap(
    swapApi(events, toasts),
    ["plugin:acme:alpha"],
    async () => {
      events.push("swap");
      return {
        plugin_id: "acme",
        from: "1.0.0",
        to: "1.1.0",
        enabled_servers: ["plugin:acme:alpha"],
      };
    },
  );
  assert.equal(info.to, "1.1.0");
  assert.deepEqual(events, [
    "disconnect:plugin:acme:alpha",
    "swap",
    "connect:plugin:acme:alpha",
  ]);
  assert.deepEqual(toasts, []);
});

test("plugin swap restarts the stopped servers when the swap fails", async () => {
  const events = [];
  const toasts = [];
  await assert.rejects(
    runPluginSwap(swapApi(events, toasts), ["plugin:acme:alpha"], async () => {
      events.push("swap");
      throw new Error("no network");
    }),
    /no network/,
  );
  assert.deepEqual(events, [
    "disconnect:plugin:acme:alpha",
    "swap",
    "connect:plugin:acme:alpha",
  ]);
  assert.deepEqual(toasts, []);
});

test("plugin swap reconnects only the ids the swap reports as enabled", async () => {
  const events = [];
  await runPluginSwap(
    swapApi(events, []),
    ["plugin:acme:alpha", "plugin:acme:beta"],
    async () => {
      events.push("swap");
      return {
        plugin_id: "acme",
        from: "1.0.0",
        to: "1.1.0",
        enabled_servers: ["plugin:acme:alpha"],
      };
    },
  );
  assert.deepEqual(events, [
    "disconnect:plugin:acme:alpha",
    "disconnect:plugin:acme:beta",
    "swap",
    "connect:plugin:acme:alpha",
  ]);
});

test("plugin swap reports a failed reconnect without failing the swap", async () => {
  const toasts = [];
  const info = await runPluginSwap(
    {
      disconnect: async () => {},
      connect: async () => {
        throw new Error("server gone");
      },
      toast: (message) => toasts.push(message),
    },
    [],
    async () => ({
      plugin_id: "acme",
      from: null,
      to: "1.1.0",
      enabled_servers: ["plugin:acme:alpha"],
    }),
  );
  assert.equal(info.to, "1.1.0");
  assert.equal(toasts.length, 1);
  assert.match(toasts[0], /server gone/);
});

test("plugin batch swap stops every plugin and restarts the reported ids", async () => {
  const events = [];
  const updates = await runPluginBatchSwap(
    swapApi(events, []),
    new Map([["acme", ["plugin:acme:alpha"]]]),
    async () => {
      events.push("swap");
      return [
        {
          plugin_id: "acme",
          from: "1.0.0",
          to: "1.1.0",
          enabled_servers: ["plugin:acme:alpha"],
        },
      ];
    },
  );
  assert.equal(updates.length, 1);
  assert.deepEqual(events, [
    "disconnect:plugin:acme:alpha",
    "swap",
    "connect:plugin:acme:alpha",
  ]);
});

test("plugin batch swap restarts a plugin the batch did not update", async () => {
  const events = [];
  await runPluginBatchSwap(
    swapApi(events, []),
    new Map([
      ["acme", ["plugin:acme:alpha"]],
      ["beta", ["plugin:beta:alpha"]],
    ]),
    async () => {
      events.push("swap");
      return [];
    },
  );
  assert.deepEqual(events, [
    "disconnect:plugin:acme:alpha",
    "disconnect:plugin:beta:alpha",
    "swap",
    "connect:plugin:acme:alpha",
    "connect:plugin:beta:alpha",
  ]);
});

test("plugin batch swap restarts everything when the batch fails", async () => {
  const events = [];
  await assert.rejects(
    runPluginBatchSwap(
      swapApi(events, []),
      new Map([["acme", ["plugin:acme:alpha"]]]),
      async () => {
        events.push("swap");
        throw new Error("no network");
      },
    ),
    /no network/,
  );
  assert.deepEqual(events, [
    "disconnect:plugin:acme:alpha",
    "swap",
    "connect:plugin:acme:alpha",
  ]);
});

test("pluginServerOwner badges plugin servers and nothing else", () => {
  assert.equal(pluginServerOwner(undefined), null);
  assert.equal(pluginServerOwner(null), null);
  assert.equal(pluginServerOwner({ kind: "user" }), null);
  assert.deepEqual(
    pluginServerOwner({ kind: "plugin", plugin_id: "acme", plugin_name: "Acme Tools" }),
    { id: "acme", name: "Acme Tools" },
  );
  // a plugin server without a display name still shows and routes by id
  assert.deepEqual(pluginServerOwner({ kind: "plugin", plugin_id: "acme" }), {
    id: "acme",
    name: "acme",
  });
  // a malformed origin is not a plugin row: never route by a missing id
  assert.equal(pluginServerOwner({ kind: "plugin", plugin_name: "Acme" }), null);
});
