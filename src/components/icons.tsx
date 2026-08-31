// Minimal inline SVG icon set.
import type { ReactElement, SVGProps } from "react";

const paths: Record<string, ReactElement> = {
  duck: (
    <>
      <circle cx="10" cy="9" r="6" fill="currentColor" opacity="0.9" />
      <path d="M15.5 9 L21 10.5 L15.5 12.5 Z" fill="currentColor" />
      <path d="M4 15 Q10 20.5 16.5 16.5 L16.5 19.5 Q10 23.5 4 18 Z" fill="currentColor" opacity="0.75" />
    </>
  ),
  chat: (
    <path
      d="M4 5.5A2.5 2.5 0 0 1 6.5 3h11A2.5 2.5 0 0 1 20 5.5v8a2.5 2.5 0 0 1-2.5 2.5H9l-4.2 3.4c-.6.5-1.3 0-1.3-.7V5.5Z"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.7"
      strokeLinejoin="round"
    />
  ),
  plug: (
    <>
      <path d="M9 3v5M15 3v5" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" />
      <path
        d="M6 8h12v3a6 6 0 0 1-12 0V8ZM12 17v4"
        stroke="currentColor"
        strokeWidth="1.7"
        strokeLinecap="round"
        strokeLinejoin="round"
        fill="none"
      />
    </>
  ),
  settings: (
    <>
      <circle cx="12" cy="12" r="3" stroke="currentColor" strokeWidth="1.7" fill="none" />
      <path
        d="M12 2.8v2.4M12 18.8v2.4M4.9 4.9l1.7 1.7M17.4 17.4l1.7 1.7M2.8 12h2.4M18.8 12h2.4M4.9 19.1l1.7-1.7M17.4 6.6l1.7-1.7"
        stroke="currentColor"
        strokeWidth="1.7"
        strokeLinecap="round"
      />
    </>
  ),
  send: <path d="M4 12 20 4l-4 16-4.5-6.5L4 12Z" fill="currentColor" />,
  stop: <rect x="6" y="6" width="12" height="12" rx="2.5" fill="currentColor" />,
  plus: <path d="M12 5v14M5 12h14" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" />,
  trash: (
    <path
      d="M5 7h14M10 7V5h4v2m-8 0 1 13h8l1-13M10 11v6M14 11v6"
      stroke="currentColor"
      strokeWidth="1.6"
      strokeLinecap="round"
      strokeLinejoin="round"
      fill="none"
    />
  ),
  refresh: (
    <path
      d="M20 12a8 8 0 1 1-2.34-5.66M20 4v4h-4"
      stroke="currentColor"
      strokeWidth="1.7"
      strokeLinecap="round"
      strokeLinejoin="round"
      fill="none"
    />
  ),
  key: (
    <>
      <circle cx="8" cy="14" r="4" stroke="currentColor" strokeWidth="1.7" fill="none" />
      <path d="M11 11 20 2m-4 2 3 3m-6 0 3 3" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" />
    </>
  ),
  check: (
    <path d="M4.5 12.5 10 18 19.5 6.5" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" fill="none" />
  ),
  x: <path d="M6 6l12 12M18 6 6 18" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" />,
  warning: (
    <>
      <path d="M12 3.5 22 20H2L12 3.5Z" stroke="currentColor" strokeWidth="1.7" strokeLinejoin="round" fill="none" />
      <path d="M12 10v4.5" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" />
      <circle cx="12" cy="17.2" r="0.9" fill="currentColor" />
    </>
  ),
  spinner: (
    <>
      <circle cx="12" cy="12" r="9" stroke="currentColor" strokeWidth="2.4" opacity="0.2" fill="none" />
      <path d="M21 12a9 9 0 0 0-9-9" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" fill="none" />
    </>
  ),
  chevron: <path d="m6 9 6 6 6-6" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" fill="none" />,
  copy: (
    <>
      <rect x="9" y="9" width="11" height="11" rx="2" stroke="currentColor" strokeWidth="1.6" fill="none" />
      <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" stroke="currentColor" strokeWidth="1.6" fill="none" />
    </>
  ),
  "external-link": (
    <>
      <path d="M14 5h5v5M19 5l-8 8" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" fill="none" />
      <path d="M19 14v4a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V7a2 2 0 0 1 2-2h4" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" fill="none" />
    </>
  ),
  folder: (
    <path
      d="M3 6a2 2 0 0 1 2-2h4l2.4 2.5H19a2 2 0 0 1 2 2V18a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V6Z"
      stroke="currentColor"
      strokeWidth="1.7"
      strokeLinejoin="round"
      fill="none"
    />
  ),
  globe: (
    <>
      <circle cx="12" cy="12" r="9" stroke="currentColor" strokeWidth="1.6" fill="none" />
      <path d="M3 12h18M12 3c2.7 2.6 4 5.7 4 9s-1.3 6.4-4 9c-2.7-2.6-4-5.7-4-9s1.3-6.4 4-9Z" stroke="currentColor" strokeWidth="1.6" fill="none" />
    </>
  ),
  terminal: (
    <>
      <rect x="3" y="4.5" width="18" height="15" rx="2.5" stroke="currentColor" strokeWidth="1.6" fill="none" />
      <path d="m7 9 3.5 3L7 15M12.5 15.5h4.5" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" fill="none" />
    </>
  ),
  shield: (
    <path
      d="M12 3 20 6v6c0 4.5-3.4 7.8-8 9-4.6-1.2-8-4.5-8-9V6l8-3Z"
      stroke="currentColor"
      strokeWidth="1.7"
      strokeLinejoin="round"
      fill="none"
    />
  ),
  wrench: (
    <path
      d="M14.5 6.5a4.5 4.5 0 0 0-6 5.7L3 17.8 6.2 21l5.6-5.5a4.5 4.5 0 0 0 5.7-6L14 13l-3-3 3.5-3.5Z"
      stroke="currentColor"
      strokeWidth="1.7"
      strokeLinejoin="round"
      fill="none"
    />
  ),
  box: (
    <>
      <path d="M12 3 21 7.5v9L12 21 3 16.5v-9L12 3Z" stroke="currentColor" strokeWidth="1.6" strokeLinejoin="round" fill="none" />
      <path d="M3 7.5 12 12l9-4.5M12 12v9" stroke="currentColor" strokeWidth="1.6" strokeLinejoin="round" fill="none" />
    </>
  ),
  file: (
    <>
      <path d="M6 3h8l4 4v14H6V3Z" stroke="currentColor" strokeWidth="1.6" strokeLinejoin="round" fill="none" />
      <path d="M14 3v4h4" stroke="currentColor" strokeWidth="1.6" strokeLinejoin="round" fill="none" />
    </>
  ),
};

export type IconName = keyof typeof paths | string;

export function Icon({
  name,
  className = "h-5 w-5",
  ...rest
}: { name: IconName; className?: string } & SVGProps<SVGSVGElement>) {
  return (
    <svg
      viewBox="0 0 24 24"
      className={className}
      aria-hidden="true"
      {...rest}
    >
      {paths[name] ?? paths.box}
    </svg>
  );
}

export function StatusDot({ status }: { status: string }) {
  const cls =
    status === "connected"
      ? "bg-emerald-500"
      : status === "connecting"
        ? "bg-amber-400 animate-pulse"
        : status === "needs_auth"
          ? "bg-amber-500"
          : status === "error"
            ? "bg-rose-500"
            : "bg-slate-400";
  return <span className={`inline-block h-2.5 w-2.5 rounded-full ${cls}`} aria-label={status} />;
}
