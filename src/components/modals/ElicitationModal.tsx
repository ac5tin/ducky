import { useEffect, useMemo, useState } from "react";
import { useStore } from "../../store";
import type { ElicitationFieldSchema, ElicitationSchemaShape } from "../../types";
import { Button, Field, inputClass } from "./Modal";
import { Icon } from "../icons";
import { openUrl } from "@tauri-apps/plugin-opener";

/** Render and collect a form-mode elicitation defined by the server's schema. */
export function ElicitationModal() {
  const queue = useStore((s) => s.elicitations);
  const respond = useStore((s) => s.respondElicitation);
  const current = queue[0];

  if (!current) return null;

  if (current.mode === "url") {
    return <UrlElicitation request={current} />;
  }
  return <FormElicitation key={current.request_id} request={current} onRespond={respond} />;
}

function FormElicitation({
  request,
  onRespond,
}: {
  request: { request_id: string; server_title: string; message: string; schema?: any };
  onRespond: (id: string, action: "accept" | "decline" | "cancel", content?: any) => void;
}) {
  const shape: ElicitationSchemaShape = request.schema ?? {};
  const properties = shape.properties ?? {};
  const required = new Set(shape.required ?? []);
  const [values, setValues] = useState<Record<string, any>>({});

  useEffect(() => {
    // pre-fill defaults
    const initial: Record<string, any> = {};
    for (const [key, field] of Object.entries(properties)) {
      if (field.default !== undefined) initial[key] = field.default;
    }
    setValues(initial);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [request.request_id]);

  const missing = useMemo(
    () => [...required].filter((k) => values[k] === undefined || values[k] === ""),
    [required, values],
  );

  return (
    <ModalShell
      title={`“${request.server_title}” is asking for information`}
      icon="shield"
    >
      <p className="mb-4 text-sm text-slate-600 dark:text-slate-300">{request.message}</p>
      <div className="space-y-3.5">
        {Object.entries(properties).map(([key, field]) => (
          <ElicitField
            key={key}
            name={key}
            required={required.has(key)}
            field={field}
            value={values[key]}
            onChange={(v) => setValues((prev) => ({ ...prev, [key]: v }))}
          />
        ))}
      </div>
      <div className="mt-6 flex items-center justify-between gap-3">
        <Button variant="ghost" onClick={() => onRespond(request.request_id, "cancel")}>
          Cancel
        </Button>
        <div className="flex gap-2">
          <Button variant="secondary" onClick={() => onRespond(request.request_id, "decline")}>
            Decline
          </Button>
          <Button
            variant="primary"
            disabled={missing.length > 0}
            title={missing.length ? `Please fill: ${missing.join(", ")}` : undefined}
            onClick={() => onRespond(request.request_id, "accept", values)}
          >
            Submit
          </Button>
        </div>
      </div>
    </ModalShell>
  );
}

function ElicitField({
  name,
  required,
  field,
  value,
  onChange,
}: {
  name: string;
  required: boolean;
  field: ElicitationFieldSchema;
  value: any;
  onChange: (v: any) => void;
}) {
  const label = (
    <>
      {field.title ?? name}
      {required && <span className="ml-1 text-rose-500">*</span>}
    </>
  );
  const options =
    field.enum ??
    field.oneOf?.map((o) => ({ value: o.const, label: o.title ?? o.const })) ??
    field.anyOf?.map((o) => ({ value: o.const, label: o.title ?? o.const }));

  if (field.type === "array" && field.items) {
    const itemOptions =
      field.items.enum ??
      field.items.anyOf?.map((o) => ({ value: o.const, label: o.title ?? o.const }));
    const selected: string[] = Array.isArray(value) ? value : [];
    return (
      <Field label={label} hint={field.description}>
        <div className="space-y-1.5">
          {(itemOptions ?? []).map((opt: any) => {
            const v = typeof opt === "string" ? opt : opt.value;
            const l = typeof opt === "string" ? opt : opt.label;
            return (
              <label key={v} className="flex items-center gap-2 text-sm">
                <input
                  type="checkbox"
                  checked={selected.includes(v)}
                  onChange={(e) =>
                    onChange(
                      e.target.checked
                        ? [...selected, v]
                        : selected.filter((x) => x !== v),
                    )
                  }
                />
                {l}
              </label>
            );
          })}
        </div>
      </Field>
    );
  }

  if (options) {
    return (
      <Field label={label} hint={field.description}>
        <select
          className={inputClass}
          value={value ?? ""}
          onChange={(e) => onChange(e.target.value || undefined)}
        >
          <option value="" disabled>
            Choose…
          </option>
          {options.map((opt: any) => {
            const v = typeof opt === "string" ? opt : opt.value;
            const l = typeof opt === "string" ? opt : opt.label;
            return (
              <option key={v} value={v}>
                {l}
              </option>
            );
          })}
        </select>
      </Field>
    );
  }

  if (field.type === "boolean") {
    return (
      <Field label={label} hint={field.description}>
        <label className="flex items-center gap-2 text-sm">
          <input
            type="checkbox"
            checked={!!value}
            onChange={(e) => onChange(e.target.checked)}
          />
          {field.default !== undefined ? `Default: ${field.default ? "on" : "off"}` : "Toggle"}
        </label>
      </Field>
    );
  }

  const inputType =
    field.type === "number" || field.type === "integer"
      ? "number"
      : field.format === "date"
        ? "date"
        : field.format === "date-time"
          ? "datetime-local"
          : field.format === "email"
            ? "email"
            : field.format === "uri"
              ? "url"
              : "text";

  return (
    <Field label={label} hint={field.description}>
      <input
        className={inputClass}
        type={inputType}
        min={field.minimum}
        max={field.maximum}
        minLength={field.minLength}
        maxLength={field.maxLength}
        value={value ?? ""}
        onChange={(e) =>
          onChange(
            field.type === "number" || field.type === "integer"
              ? e.target.value === ""
                ? undefined
                : Number(e.target.value)
              : e.target.value === ""
                ? undefined
                : e.target.value,
          )
        }
      />
    </Field>
  );
}

/** URL-mode elicitation: consent, show the full target, hand off to the browser. */
function UrlElicitation({
  request,
}: {
  request: { request_id: string; server_title: string; message: string; url?: string };
}) {
  const respond = useStore((s) => s.respondElicitation);
  const [opened, setOpened] = useState(false);
  const url = request.url ?? "";
  let domain = url;
  try {
    domain = new URL(url).host;
  } catch {
    /* keep raw */
  }

  return (
    <ModalShell title={`“${request.server_title}” needs your attention`} icon="globe">
      <p className="mb-3 text-sm text-slate-600 dark:text-slate-300">{request.message}</p>
      <div className="rounded-xl border border-amber-200 bg-amber-50 p-3 text-sm dark:border-amber-900 dark:bg-amber-950/40">
        <div className="mb-1 flex items-center gap-2 font-medium text-amber-800 dark:text-amber-200">
          <Icon name="warning" className="h-4 w-4" />
          This opens an external site
        </div>
        <p className="mb-2 text-amber-700 dark:text-amber-300">
          You'll visit <span className="font-semibold">{domain}</span>. Ducky cannot see
          what you enter there — that's the point.
        </p>
        <code className="block max-h-20 overflow-y-auto break-all rounded-md bg-white/70 px-2 py-1.5 font-mono text-xs text-slate-600 dark:bg-slate-900/60 dark:text-slate-300">
          {url}
        </code>
      </div>
      <div className="mt-5 flex justify-between gap-3">
        <Button variant="ghost" onClick={() => respond(request.request_id, "cancel")}>
          Cancel
        </Button>
        <div className="flex gap-2">
          <Button variant="secondary" onClick={() => respond(request.request_id, "decline")}>
            Decline
          </Button>
          <Button
            variant="primary"
            onClick={async () => {
              await openUrl(url).catch(() => {});
              setOpened(true);
              respond(request.request_id, "accept");
            }}
          >
            <Icon name="external-link" className="h-4 w-4" />
            Open in browser
          </Button>
        </div>
      </div>
      {opened && (
        <p className="mt-3 text-xs text-slate-400">
          Finish in your browser, then return to Ducky — the connector will pick up where
          it left off.
        </p>
      )}
    </ModalShell>
  );
}

function ModalShell({
  title,
  icon,
  children,
}: {
  title: string;
  icon: string;
  children: React.ReactNode;
}) {
  return (
    <div className="fixed inset-0 z-[60] flex items-center justify-center p-6">
      <div className="fade-in absolute inset-0 bg-slate-950/50 backdrop-blur-[2px]" />
      <div className="pop-in relative w-full max-w-lg overflow-hidden rounded-2xl border border-slate-200 bg-white shadow-2xl dark:border-slate-700 dark:bg-slate-900">
        <div className="flex items-center gap-2.5 border-b border-slate-100 px-5 py-4 dark:border-slate-800">
          <span className="flex h-8 w-8 items-center justify-center rounded-lg bg-sky-100 text-sky-600 dark:bg-sky-900/50 dark:text-sky-300">
            <Icon name={icon} className="h-4.5 w-4.5" />
          </span>
          <h2 className="text-base font-semibold">{title}</h2>
        </div>
        <div className="max-h-[70vh] overflow-y-auto px-5 py-4">{children}</div>
      </div>
    </div>
  );
}
