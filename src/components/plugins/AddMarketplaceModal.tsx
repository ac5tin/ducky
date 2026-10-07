import { useEffect, useState } from "react";
import { useStore } from "../../store";
import { Button, Field, Modal, inputClass } from "../modals/Modal";

/**
 * Add one marketplace. The backend parses the source; a parse failure is
 * shown inline (the store also toasts it), so the field is never a dead end.
 */
export function AddMarketplaceModal({
  open,
  onClose,
}: {
  open: boolean;
  onClose: () => void;
}) {
  const addMarketplace = useStore((s) => s.addMarketplace);
  const [source, setSource] = useState("");
  const [path, setPath] = useState("");
  const [name, setName] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (open) {
      setSource("");
      setPath("");
      setName("");
      setError(null);
    }
  }, [open]);

  const submit = async () => {
    setError(null);
    setBusy(true);
    try {
      await addMarketplace(
        { source: source.trim(), path: path.trim() || null },
        name.trim() || undefined,
      );
      onClose();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      open={open}
      onClose={onClose}
      title="Add a marketplace"
      subtitle="A registry of plugins Ducky can install from."
    >
      <div className="space-y-4">
        <Field
          label="Source"
          hint="owner/repo, a git URL, an HTTPS registry URL, or a local path."
        >
          <input
            className={inputClass}
            value={source}
            onChange={(e) => setSource(e.target.value)}
            placeholder="microsoft/Agents"
            autoFocus
          />
        </Field>
        <Field
          label="Subdirectory"
          hint="Optional. The folder inside the repository that holds the registry — for example microsoft/Agents keeps it in agent-plugins/. A local path or registry URL ignores this."
        >
          <input
            className={inputClass}
            value={path}
            onChange={(e) => setPath(e.target.value)}
            placeholder="agent-plugins"
          />
        </Field>
        <Field
          label="Display name"
          hint="Optional. Defaults to the repository or folder name."
        >
          <input
            className={inputClass}
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="Microsoft Agents"
          />
        </Field>
        {error && (
          <p className="rounded-lg bg-rose-50 px-3 py-2 text-xs text-rose-700 dark:bg-rose-950/40 dark:text-rose-300">
            {error}
          </p>
        )}
        <div className="flex justify-end gap-2">
          <Button variant="secondary" onClick={onClose}>
            Cancel
          </Button>
          <Button
            disabled={busy || source.trim().length === 0}
            onClick={() => void submit()}
          >
            Add marketplace
          </Button>
        </div>
      </div>
    </Modal>
  );
}
