// Composer `@` file references: pure helpers for the trigger token, the
// candidate filter, and the inserted binding. The menu and keyboard handling
// live in ChatView, like the `/` and `#` menus. The model reads the file on
// demand via ducky__fs_read (see ADR-0006 for the `#` precedent).

/** Files offered before the user narrows by typing. */
const MAX_MATCHES = 20;

/** An `@` token the caret sits in. */
export interface FileReferenceToken {
  /** Index of the `@` that starts the token. */
  start: number;
  /** Text typed between the `@` and the caret. */
  query: string;
}

/**
 * The `@` token at `caret`, or null. A token starts at an `@` that opens the
 * text or follows whitespace, contains no whitespace or further `@` before
 * the caret, and holds at least one non-`@` character — so `a@b.com`,
 * `issue@42` and a finished `@a b` stay ordinary text.
 */
export function fileReferenceToken(
  text: string,
  caret: number,
): FileReferenceToken | null {
  const cursor = Math.max(0, Math.min(caret, text.length));
  if (cursor === 0) return null;
  const at = text.lastIndexOf("@", cursor - 1);
  if (at < 0) return null;
  if (at > 0 && !/\s/.test(text[at - 1] ?? "")) return null;
  const query = text.slice(at + 1, cursor);
  if (/\s|@/.test(query)) return null;
  return { start: at, query };
}

/**
 * Paths the menu shows for a query: case-insensitive substring match on the
 * whole relative path, files before directories, capped.
 */
export function matchFileReferences(files: string[], query: string): string[] {
  const q = query.trim().toLowerCase();
  return files
    .filter((path) => q === "" || path.toLowerCase().includes(q))
    .sort(
      (a, b) =>
        Number(a.endsWith("/")) - Number(b.endsWith("/")) ||
        (a < b ? -1 : Number(a > b)),
    )
    .slice(0, MAX_MATCHES);
}

/** The text an accepted row inserts at the token. */
export function fileReferenceInsertion(path: string): string {
  // paths with spaces cannot be one word-anchored token, so quote them
  return /\s/.test(path) ? `@"${path}" ` : `@${path} `;
}
