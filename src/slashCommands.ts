// Slash commands typed in the composer ("/compact", "/undo", "/init").
// Pure logic only — the store routes parsed commands to their actions.

export interface SlashCommand {
  name: string;
  description: string;
  /** Commands that operate on a transcript need an open conversation. */
  needsConversation: boolean;
}

export const SLASH_COMMANDS: SlashCommand[] = [
  {
    name: "compact",
    description: "Summarise the conversation so far to free up context",
    needsConversation: true,
  },
  {
    name: "undo",
    description: "Undo the last turn and revert its file changes",
    needsConversation: true,
  },
  {
    name: "init",
    description: "Create or update AGENTS.md for the working directory",
    needsConversation: false,
  },
];

/** Marks the user message that carries a compaction summary. Must match
 * `COMPACT_MARKER` in src-tauri/src/compact.rs. */
export const COMPACT_SUMMARY_MARKER = "[Conversation compacted]";

/** First line of the /init prompt; identifies it in the transcript. */
export const INIT_PROMPT_MARKER = "[/init]";

export interface ParsedSlashCommand {
  name: string;
  args: string;
  /** A command-shaped token ("/hello") that matches no known command. */
  unknown: boolean;
}

const COMMAND_TOKEN = /^\/([A-Za-z][\w-]*)$/;

/**
 * Parse `text` as a slash command, or null when it is a normal message.
 * Only a bare first token counts ("/compact", "/compact focus on x");
 * anything whose first token contains more slashes ("/home/x is broken")
 * is left alone so pasted paths still send as chat.
 */
export function parseSlashCommand(text: string): ParsedSlashCommand | null {
  const trimmed = text.trim();
  if (!trimmed.startsWith("/")) return null;
  const firstToken = trimmed.split(/\s+/, 1)[0];
  const match = COMMAND_TOKEN.exec(firstToken);
  if (!match) return null;
  const name = match[1].toLowerCase();
  const args = trimmed.slice(firstToken.length).trim();
  return {
    name,
    args,
    unknown: !SLASH_COMMANDS.some((c) => c.name === name),
  };
}

/** Commands whose name starts with the token being typed ("/co" → compact). */
export function filterCommands(token: string): SlashCommand[] {
  const q = token.trim().replace(/^\//, "").toLowerCase();
  return SLASH_COMMANDS.filter((c) => c.name.startsWith(q));
}

/**
 * The prompt /init sends: an ordinary user message (the model explores with
 * its own file tools and writes AGENTS.md), adapted from opencode's
 * `initialize.txt` template.
 */
export function expandInitPrompt(workingDir: string, args: string): string {
  const prompt = `${INIT_PROMPT_MARKER} Create or update AGENTS.md for this repository.

Working directory: ${workingDir}

The goal is a compact instruction file that helps future AI coding sessions in this directory avoid mistakes and ramp up quickly. Every line should answer: "Would an agent likely miss this without help?" If not, leave it out.

How to investigate:
- Read the highest-value sources first: README, root manifests (package.json, Cargo.toml, pyproject.toml…), build/CI config, and any existing instruction files (AGENTS.md, CLAUDE.md, .cursor/rules…).
- Prefer executable sources of truth (configs, scripts, CI) over prose docs when they disagree.
- Use the file tools to look around; don't guess.

What to extract:
- The exact commands to install dependencies, build, run, and test — including how to run a single test.
- Toolchain quirks: required versions, package manager (npm vs pnpm…), platform prerequisites.
- Layout: where the source lives, where tests go, key modules.
- Rules and gotchas a contributor must not break (secrets handling, code that must not be refactored, intentionally silenced warnings).

Writing rules:
- Short and specific. No generic advice, no exhaustive file trees, no speculation — when in doubt, omit.
- Show commands as code blocks.

If AGENTS.md already exists in the working directory, improve it in place rather than rewriting blindly: preserve verified useful guidance, delete fluff or stale claims.

Write the result to AGENTS.md in the working directory.`;
  if (!args.trim()) return prompt;
  return `${prompt}

Extra instructions from the user:
${args.trim()}`;
}
