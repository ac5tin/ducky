import { useStore } from "../store";
import { Icon } from "./icons";

export function Toasts() {
  const toasts = useStore((s) => s.toasts);
  const dismiss = useStore((s) => s.dismissToast);
  return (
    <div className="pointer-events-none fixed bottom-4 right-4 z-[100] flex w-96 max-w-[calc(100vw-2rem)] flex-col gap-2">
      {toasts.map((t) => (
        <div
          key={t.id}
          className={`pop-in pointer-events-auto flex items-start gap-2.5 rounded-xl border px-4 py-3 text-sm shadow-lg ${
            t.kind === "error"
              ? "border-rose-200 bg-rose-50 text-rose-800 dark:border-rose-900 dark:bg-rose-950 dark:text-rose-200"
              : t.kind === "success"
                ? "border-emerald-200 bg-emerald-50 text-emerald-800 dark:border-emerald-900 dark:bg-emerald-950 dark:text-emerald-200"
                : "border-slate-200 bg-white text-slate-700 dark:border-slate-700 dark:bg-slate-900 dark:text-slate-200"
          }`}
        >
          <Icon
            name={t.kind === "error" ? "warning" : t.kind === "success" ? "check" : "chat"}
            className={`mt-0.5 h-4 w-4 shrink-0 ${t.kind === "error" ? "text-rose-500" : "text-emerald-500"}`}
          />
          <span className="flex-1 leading-snug">{t.text}</span>
          <button
            className="opacity-50 transition hover:opacity-100"
            aria-label="Dismiss"
            onClick={() => dismiss(t.id)}
          >
            <Icon name="x" className="h-4 w-4" />
          </button>
        </div>
      ))}
    </div>
  );
}
