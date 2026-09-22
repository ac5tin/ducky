import { useEffect, useRef, useState } from "react";
import { GROUP_COLORS, normalizeGroupColor } from "../../groups";
import { useStore } from "../../store";
import type { ChatGroup } from "../../types";

export function GroupColorPicker({
  group,
  onClose,
}: {
  group: ChatGroup;
  onClose: () => void;
}) {
  const setGroupColor = useStore((s) => s.setGroupColor);
  const current = normalizeGroupColor(group.color);
  // React's onChange is the live `input` event, so the wheel fires it on every
  // movement. The wheel therefore edits a local draft, and the colour is
  // committed once, when the input loses focus or the popover closes.
  const [draft, setDraft] = useState(current);
  const dirty = useRef(false);

  const commit = () => {
    if (!dirty.current) return;
    dirty.current = false;
    setGroupColor(group.id, draft).catch((e) => console.error(e));
  };

  const close = () => {
    commit();
    onClose();
  };

  // The listener subscribes once per mount, so it reaches the newest `close`
  // through a ref — `close` closes over `draft`, which changes as the wheel moves.
  const closeRef = useRef(close);
  closeRef.current = close;

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") closeRef.current();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const pickPreset = (color: string) => {
    setGroupColor(group.id, color).catch((e) => console.error(e));
    onClose();
  };

  return (
    <>
      <div className="fixed inset-0 z-30" onClick={close} />
      <div className="pop-in absolute left-0 top-full z-40 mt-1 w-52 rounded-xl border border-slate-200 bg-white p-2 shadow-xl dark:border-slate-700 dark:bg-slate-900">
        <div className="flex items-center gap-1.5">
          {GROUP_COLORS.map((color) => (
            <button
              key={color}
              className={`h-5 w-5 rounded-full ring-2 transition ${
                color === current ? "ring-sky-400" : "ring-transparent"
              }`}
              style={{ backgroundColor: color }}
              aria-label={`Group colour ${color}`}
              title={color}
              onClick={() => pickPreset(color)}
            />
          ))}
        </div>
        <label className="mt-2 flex items-center justify-between gap-2 text-xs text-slate-500">
          Custom
          <input
            type="color"
            className="h-6 w-9 cursor-pointer rounded border border-slate-200 bg-transparent p-0 dark:border-slate-700"
            value={draft}
            aria-label="Custom group colour"
            onChange={(e) => {
              dirty.current = true;
              setDraft(e.target.value);
            }}
            onBlur={commit}
          />
        </label>
      </div>
    </>
  );
}
