// Composer `#` chat references: pure helpers for the trigger token, the
// candidate list, and the inserted binding. The menu and keyboard handling
// live in ChatView, like the `/` command menu.

import type { ConversationMeta } from "./types";

/** Chats offered before the user narrows by typing. */
const MAX_MATCHES = 20;

/** A `#` token the caret sits in. */
export interface SessionReferenceToken {
  /** Index of the `#` that starts the token. */
  start: number;
  /** Text typed between the `#` and the caret. */
  query: string;
}

/**
 * The `#` token at `caret`, or null. A token starts at a `#` that opens the
 * text or follows whitespace and contains no whitespace before the caret —
 * so `C#`, `foo#bar` and a finished `#a b` stay ordinary text.
 */
export function sessionReferenceToken(
  text: string,
  caret: number,
): SessionReferenceToken | null {
  const cursor = Math.max(0, Math.min(caret, text.length));
  if (cursor === 0) return null;
  const hash = text.lastIndexOf("#", cursor - 1);
  if (hash < 0) return null;
  if (hash > 0 && !/\s/.test(text[hash - 1] ?? "")) return null;
  const query = text.slice(hash + 1, cursor);
  if (/\s|#/.test(query)) return null;
  return { start: hash, query };
}

/** Chats the menu shows: active chat excluded, newest first, title-filtered. */
export function sessionReferenceMatches(
  conversations: ConversationMeta[],
  query: string,
  activeId: string | null,
): ConversationMeta[] {
  const q = query.trim().toLowerCase();
  return conversations
    .filter((chat) => chat.id !== activeId)
    .filter((chat) => q === "" || chat.title.toLowerCase().includes(q))
    .sort(
      (a, b) =>
        b.updated_at.localeCompare(a.updated_at) ||
        a.title.localeCompare(b.title),
    )
    .slice(0, MAX_MATCHES);
}

/** The text an accepted row inserts at the token. */
export function sessionReferenceInsertion(conversationId: string): string {
  return `#chat_${conversationId} `;
}
