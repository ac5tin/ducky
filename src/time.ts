function pad(n: number): string {
  return String(n).padStart(2, "0");
}

const RELATIVE_MS = 15 * 60 * 1000;

export function shortTime(iso: string, now = new Date()): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  const ageMs = now.getTime() - d.getTime();
  if (ageMs >= 0 && ageMs < RELATIVE_MS) {
    return new Intl.RelativeTimeFormat(undefined, { numeric: "auto" }).format(
      -Math.floor(ageMs / 60_000),
      "minute",
    );
  }
  const sameDay = d.toDateString() === now.toDateString();
  const sameYear = d.getFullYear() === now.getFullYear();
  const opts: Intl.DateTimeFormatOptions = {
    hour: "numeric",
    minute: "2-digit",
  };
  if (!sameDay) {
    opts.month = "short";
    opts.day = "numeric";
    if (!sameYear) opts.year = "numeric";
  }
  return new Intl.DateTimeFormat(undefined, opts).format(d);
}

export function rfc9557(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const tz = Intl.DateTimeFormat().resolvedOptions().timeZone;
  const off = -d.getTimezoneOffset();
  const sign = off >= 0 ? "+" : "-";
  const abs = Math.abs(off);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}${sign}${pad(Math.floor(abs / 60))}:${pad(abs % 60)}[${tz}]`;
}
