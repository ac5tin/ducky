import type { AgentMode, AppConfig, ConversationMeta } from "./types";

/** Cycle order for Shift+Tab and the picker. */
export const MODE_ORDER: AgentMode[] = ["default", "readonly", "plan", "auto"];

/**
 * User-facing copy per mode. The read-only descriptions say which tools run,
 * never that nothing can change: a server's read-only mark is trusted as given
 * (docs/adr/0004-trust-mcp-readonly-hint-for-readonly-modes.md).
 */
export const MODE_META: Record<
  AgentMode,
  { label: string; description: string; icon: string }
> = {
  default: {
    label: "Default",
    description:
      "No restrictions. Every tool is available, with the approval rules from Settings.",
    icon: "duck",
  },
  readonly: {
    label: "Read-only",
    description:
      "Only tools marked read-only run; everything else is refused. A server's read-only mark is trusted as given.",
    icon: "shield",
  },
  plan: {
    label: "Plan",
    description:
      "Read-only research first, then a plan you review. Approving it starts the work in Default mode.",
    icon: "file",
  },
  auto: {
    label: "Auto",
    description:
      "Starts unrestricted. The agent switches itself to Plan or Read-only when the task calls for it.",
    icon: "refresh",
  },
};

export function nextMode(mode: AgentMode): AgentMode {
  const i = MODE_ORDER.indexOf(mode);
  return MODE_ORDER[(i + 1) % MODE_ORDER.length];
}

/**
 * The mode to show: the chat's own when one is open, otherwise the staged
 * draft pick, otherwise the app default.
 */
export function resolveShownMode(
  conversation: Pick<ConversationMeta, "mode"> | undefined,
  draftMode: AgentMode | null,
  config: AppConfig | null,
): AgentMode {
  if (conversation) return conversation.mode;
  if (draftMode) return draftMode;
  return config?.settings.default_mode ?? "default";
}