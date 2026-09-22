import { useRef, useState } from "react";
import type { ChangeEvent, KeyboardEvent, MouseEvent } from "react";

/** Inline rename behaviour shared by chat rows and group headers: Enter
 *  commits, Escape cancels, blur commits. Same rules as the chat rename that
 *  used to live in Sidebar.tsx. */
export function useInlineRename(initial: string, onCommit: (title: string) => void) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(initial);
  const skipBlur = useRef(false);

  const start = () => {
    setDraft(initial);
    setEditing(true);
  };

  const cancel = () => {
    skipBlur.current = true;
    setEditing(false);
  };

  const commit = () => {
    const trimmed = draft.trim();
    if (trimmed && trimmed !== initial) onCommit(trimmed);
    setEditing(false);
  };

  return {
    editing,
    start,
    cancel,
    inputProps: {
      autoFocus: true,
      value: draft,
      onChange: (e: ChangeEvent<HTMLInputElement>) => setDraft(e.target.value),
      onClick: (e: MouseEvent) => e.stopPropagation(),
      onKeyDown: (e: KeyboardEvent<HTMLInputElement>) => {
        if (e.key === "Enter") {
          e.preventDefault();
          skipBlur.current = true;
          commit();
        } else if (e.key === "Escape") {
          e.preventDefault();
          cancel();
        }
      },
      onBlur: () => {
        if (skipBlur.current) {
          skipBlur.current = false;
          return;
        }
        commit();
      },
    },
  };
}