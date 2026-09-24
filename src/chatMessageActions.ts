import {
  COMPACT_SUMMARY_MARKER,
  INIT_PROMPT_MARKER,
} from "./slashCommands.ts";

export type MessageActionKind = "copy" | "edit";

export type MessageActionItem = {
  kind: "user" | "assistant" | "tool";
  text: string;
  messageIndex?: number;
  streaming?: boolean;
};

export function messageActionKinds(
  item: MessageActionItem,
): MessageActionKind[] {
  if (item.kind === "user") {
    if (
      !item.text ||
      item.text.startsWith(COMPACT_SUMMARY_MARKER) ||
      item.text.startsWith(INIT_PROMPT_MARKER)
    ) {
      return [];
    }
    return item.messageIndex === undefined ? ["copy"] : ["copy", "edit"];
  }
  if (item.kind === "assistant" && item.text && !item.streaming) {
    return ["copy"];
  }
  return [];
}
