import { open } from "@tauri-apps/plugin-dialog";
import { useStore, resolvedChatWorkingDir } from "../../store";
import { Icon } from "../icons";

function basename(path: string): string {
  const parts = path.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] ?? path;
}

export function WorkingDirChip() {
  const homeDir = useStore((s) => s.homeDir);
  const resolved = useStore(resolvedChatWorkingDir);
  const setWorkingDir = useStore((s) => s.setWorkingDir);
  const toast = useStore((s) => s.toast);

  const current = resolved ?? homeDir;
  if (!current) return null;

  const change = async () => {
    const path = await open({ directory: true, defaultPath: current });
    if (typeof path === "string" && path !== current) {
      await setWorkingDir(path).catch((e) => toast("error", `${e}`));
    }
  };

  return (
    <button
      className="flex max-w-44 items-center gap-1.5 rounded-lg px-2 py-1.5 text-xs text-slate-400 transition hover:bg-slate-100 dark:hover:bg-slate-800"
      title={`${current}${resolved ? "" : " (home)"}`}
      onClick={() => change().catch((e) => console.error(e))}
    >
      <Icon name="folder" className="h-3.5 w-3.5 shrink-0" />
      <span className="truncate">{basename(current)}</span>
    </button>
  );
}
