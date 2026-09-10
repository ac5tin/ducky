import { useEffect, useState } from "react";
import { allMcpIds, nextMcpIds } from "../../chatMcpIds";
import { useStore } from "../../store";
import { ServerDetailModal } from "../connectors/ServerDetailModal";
import { Button, Modal } from "../modals/Modal";

export function ChatConnectorsDialog({
  open,
  onClose,
}: {
  open: boolean;
  onClose: () => void;
}) {
  const servers = useStore((s) => s.servers);
  const mcpIds = useStore(
    (s) =>
      s.config?.conversations.find((c) => c.id === s.activeConversationId)
        ?.mcp_ids ?? null,
  );
  const setActiveMcpIds = useStore((s) => s.setActiveMcpIds);
  const toast = useStore((s) => s.toast);
  const [detailId, setDetailId] = useState<string | null>(null);

  useEffect(() => {
    if (!open) setDetailId(null);
  }, [open]);

  const enabled = servers.filter((s) => s.enabled);
  const enabledIds = enabled.map((s) => s.id);
  const isOn = (id: string) => mcpIds == null || mcpIds.includes(id);

  const save = (next: string[] | null) => {
    setActiveMcpIds(next).catch((e) => toast("error", `${e}`));
  };

  return (
    <>
      <Modal
        open={open}
        onClose={onClose}
        title="Connectors"
        subtitle="Choose which connectors this chat can use."
      >
        {enabled.length === 0 ? (
          <p className="text-sm text-slate-500 dark:text-slate-400">
            No enabled connectors. Turn them on in the Connectors page.
          </p>
        ) : (
          <>
            <div className="mb-3 flex gap-2">
              <Button
                variant="secondary"
                onClick={() => save(allMcpIds(enabledIds, true))}
              >
                All on
              </Button>
              <Button
                variant="secondary"
                onClick={() => save(allMcpIds(enabledIds, false))}
              >
                All off
              </Button>
            </div>
            <div className="space-y-2">
              {enabled.map((s) => (
                <div
                  key={s.id}
                  className="flex items-center gap-3 rounded-xl border border-slate-200 px-3 py-2 dark:border-slate-800"
                >
                  <span className="min-w-0 flex-1 truncate font-medium">
                    {s.name}
                  </span>
                  <Button variant="ghost" onClick={() => setDetailId(s.id)}>
                    Manage
                  </Button>
                  <label
                    className="cursor-pointer"
                    title={
                      isOn(s.id)
                        ? "Disable for this chat"
                        : "Enable for this chat"
                    }
                  >
                    <input
                      type="checkbox"
                      className="peer sr-only"
                      checked={isOn(s.id)}
                      onChange={() =>
                        save(nextMcpIds(mcpIds, enabledIds, s.id, !isOn(s.id)))
                      }
                    />
                    <span className="relative block h-5.5 w-10 rounded-full bg-slate-300 transition peer-checked:bg-sky-500 after:absolute after:left-0.5 after:top-0.5 after:h-4.5 after:w-4.5 after:rounded-full after:bg-white after:transition peer-checked:after:translate-x-4 dark:bg-slate-700" />
                  </label>
                </div>
              ))}
            </div>
          </>
        )}
      </Modal>
      <ServerDetailModal
        serverId={detailId}
        onClose={() => setDetailId(null)}
      />
    </>
  );
}
