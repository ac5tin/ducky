import test from "node:test";
import assert from "node:assert/strict";

import {
  COMPACT_SUMMARY_MARKER,
  INIT_PROMPT_MARKER,
  SLASH_COMMANDS,
  expandInitPrompt,
  filterCommands,
  parseSlashCommand,
} from "./slashCommands.ts";

test("parseSlashCommand recognises known commands with and without args", () => {
  assert.deepEqual(parseSlashCommand("/compact"), {
    name: "compact",
    args: "",
    unknown: false,
  });
  assert.deepEqual(parseSlashCommand("/undo now"), {
    name: "undo",
    args: "now",
    unknown: false,
  });
  assert.deepEqual(parseSlashCommand("  /init   focus on testing  "), {
    name: "init",
    args: "focus on testing",
    unknown: false,
  });
  // case-insensitive command names
  assert.equal(parseSlashCommand("/Compact")?.name, "compact");
});

test("parseSlashCommand flags unknown command-like tokens", () => {
  const parsed = parseSlashCommand("/hello world");
  assert.deepEqual(parsed, { name: "hello", args: "world", unknown: true });
  assert.equal(parseSlashCommand("/x")?.unknown, true);
  assert.equal(parseSlashCommand("/foo-bar")?.unknown, true);
});

test("parseSlashCommand leaves normal messages and paths alone", () => {
  assert.equal(parseSlashCommand("hello /compact"), null);
  assert.equal(parseSlashCommand("what does /compact do?"), null);
  // paths contain extra slashes in the first token → not a command
  assert.equal(parseSlashCommand("/home/user/notes is broken"), null);
  assert.equal(parseSlashCommand("/etc/hosts"), null);
  // a lone slash or digits-only token is not command-shaped
  assert.equal(parseSlashCommand("/"), null);
  assert.equal(parseSlashCommand("/42"), null);
  // multi-line input still parses by its first line
  assert.equal(parseSlashCommand("/compact\nsome note")?.name, "compact");
});

test("filterCommands matches by name prefix", () => {
  assert.deepEqual(
    filterCommands("/").map((c) => c.name),
    SLASH_COMMANDS.map((c) => c.name),
  );
  assert.deepEqual(
    filterCommands("/c").map((c) => c.name),
    ["compact"],
  );
  assert.deepEqual(
    filterCommands("/in").map((c) => c.name),
    ["init"],
  );
  assert.deepEqual(filterCommands("/zzz"), []);
});

test("expandInitPrompt embeds the working dir and user args", () => {
  const prompt = expandInitPrompt("/repos/ducky", "");
  assert.ok(prompt.startsWith(INIT_PROMPT_MARKER));
  assert.ok(prompt.includes("Working directory: /repos/ducky"));
  assert.ok(prompt.includes("AGENTS.md"));
  assert.ok(!prompt.includes("Extra instructions"));

  const withArgs = expandInitPrompt("~", "keep the testing section");
  assert.ok(withArgs.includes("Extra instructions from the user:"));
  assert.ok(withArgs.includes("keep the testing section"));
});

test("markers used for transcript rendering are stable", () => {
  // the backend writes these exact prefixes (see compact.rs / slashCommands.ts)
  assert.equal(COMPACT_SUMMARY_MARKER, "[Conversation compacted]");
  assert.equal(INIT_PROMPT_MARKER, "[/init]");
});
