import { useStore } from "../../store";
import { Button, Modal } from "./Modal";
import { Icon } from "../icons";

function formatMb(bytes: number) {
  return `${(bytes / 1_000_000).toFixed(1)} MB`;
}

/** New-release prompt: download (or auto-download progress), then restart. */
export function UpdateModal() {
  const update = useStore((s) => s.update);
  const version = useStore((s) => s.version);
  const download = useStore((s) => s.downloadUpdate);
  const restart = useStore((s) => s.restartForUpdate);
  const dismiss = useStore((s) => s.dismissUpdate);

  const visible =
    (update.status === "available" ||
      update.status === "downloading" ||
      update.status === "ready") &&
    !update.promptDismissed;
  if (!visible) return null;

  const pct =
    update.contentLength && update.contentLength > 0
      ? Math.min(100, Math.round((update.downloaded / update.contentLength) * 100))
      : null;

  return (
    <Modal
      open
      onClose={dismiss}
      title={
        update.status === "downloading"
          ? "Downloading update…"
          : update.status === "ready"
            ? "Update ready"
            : "Update available"
      }
      subtitle={
        update.status === "downloading"
          ? `Ducky v${update.version}`
          : `Ducky v${update.version} is out — you are running v${version || "?"}`
      }
    >
      {update.status === "available" && (
        <>
          {update.notes && (
            <div className="mb-4 max-h-64 overflow-auto rounded-lg bg-slate-100 p-3 text-sm leading-relaxed whitespace-pre-wrap text-slate-600 dark:bg-slate-800 dark:text-slate-300">
              {update.notes}
            </div>
          )}
          <div className="flex justify-end gap-2">
            <Button variant="secondary" onClick={dismiss}>
              Later
            </Button>
            <Button variant="primary" onClick={() => void download()}>
              Download &amp; install
            </Button>
          </div>
        </>
      )}

      {update.status === "downloading" && (
        <div>
          <div className="mb-2 flex items-center gap-2 text-sm text-slate-500 dark:text-slate-400">
            <Icon name="refresh" className="h-4 w-4 animate-spin" />
            <span>
              {pct !== null
                ? `${pct}% of ${formatMb(update.contentLength ?? 0)}`
                : `${formatMb(update.downloaded)} downloaded`}
            </span>
          </div>
          <div className="h-2 w-full overflow-hidden rounded-full bg-slate-100 dark:bg-slate-800">
            <div
              className="h-full rounded-full bg-sky-600 transition-all"
              style={{ width: pct !== null ? `${pct}%` : "100%" }}
            />
          </div>
          <p className="mt-3 text-xs text-slate-400">
            Ducky will ask to restart once the download finishes.
          </p>
        </div>
      )}

      {update.status === "ready" && (
        <>
          <p className="mb-4 text-sm text-slate-600 dark:text-slate-300">
            Ducky v{update.version} has been downloaded and installed. Restart
            to finish switching to it.
          </p>
          <div className="flex justify-end gap-2">
            <Button variant="secondary" onClick={dismiss}>
              Later
            </Button>
            <Button variant="primary" onClick={() => void restart()}>
              Restart now
            </Button>
          </div>
        </>
      )}
    </Modal>
  );
}
