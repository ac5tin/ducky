import { useEffect, useState } from "react";
import { useStore } from "../../store";
import { Modal } from "../modals/Modal";

/** The chat's ⋯ menu → Skills: every skill the model can use right now. */
export function SkillsDialog({
  open,
  onClose,
}: {
  open: boolean;
  onClose: () => void;
}) {
  const skills = useStore((s) => s.skills);
  const refreshSkills = useStore((s) => s.refreshSkills);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Refresh on open so the list is current; until it settles, neither the
  // list nor the empty sentence is shown — the slice starts empty on a fresh
  // app, and "No skills are installed" would be a lie mid-flight.
  useEffect(() => {
    if (!open) return;
    setError(null);
    setLoading(true);
    refreshSkills()
      .catch((err) => setError(String(err)))
      .finally(() => setLoading(false));
  }, [open, refreshSkills]);

  return (
    <Modal
      open={open}
      onClose={onClose}
      title="Skills"
      subtitle="The skills available right now."
    >
      {loading ? (
        <p className="text-sm text-slate-500 dark:text-slate-400">
          Loading skills…
        </p>
      ) : error ? (
        <p className="text-sm text-rose-600 dark:text-rose-400">
          Could not list skills: {error}
        </p>
      ) : skills.length === 0 ? (
        <p className="text-sm text-slate-500 dark:text-slate-400">
          No skills are installed and enabled.
        </p>
      ) : (
        <div className="space-y-2">
          {skills.map((skill) => (
            <div
              key={skill.id}
              className="rounded-xl border border-slate-200 px-3 py-2 dark:border-slate-800"
            >
              <div className="flex flex-wrap items-center gap-1.5">
                <span className="font-mono text-sm font-medium">{skill.id}</span>
                <span className={chipClass}>{skill.origin}</span>
                {skill.shadowed && (
                  <span className={chipClass}>
                    Shadowed by {skill.shadowed}
                  </span>
                )}
              </div>
              <p className="mt-0.5 text-xs leading-relaxed text-slate-500 dark:text-slate-400">
                {skill.description}
              </p>
            </div>
          ))}
        </div>
      )}
    </Modal>
  );
}

const chipClass =
  "rounded-full bg-slate-100 px-2 py-0.5 text-[11px] text-slate-500 dark:bg-slate-800 dark:text-slate-400";