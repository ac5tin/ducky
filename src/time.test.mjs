import assert from "node:assert/strict";
import test from "node:test";
import { rfc9557, shortTime } from "./time.ts";

test("rfc9557 uses the machine zone and offset", () => {
  const s = rfc9557("2026-01-15T12:00:00Z");
  const tz = Intl.DateTimeFormat().resolvedOptions().timeZone;
  assert.match(
    s,
    /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}[+-]\d{2}:\d{2}\[[A-Za-z0-9_/+-]+\]$/,
  );
  assert.equal(s.endsWith(`[${tz}]`), true);
});

test("shortTime omits the date when the local day matches now", () => {
  const now = new Date(2026, 5, 1, 15, 0, 0);
  const same = new Date(2026, 5, 1, 8, 4, 0);
  const other = new Date(2026, 5, 2, 8, 4, 0);
  const a = shortTime(same.toISOString(), now);
  const b = shortTime(other.toISOString(), now);
  assert.ok(a.length > 0);
  assert.ok(b.length > a.length);
});

test("invalid instants do not throw", () => {
  assert.equal(shortTime("nope"), "");
  assert.equal(rfc9557("nope"), "nope");
});

test("shortTime uses relative minutes within 15 minutes", () => {
  const now = new Date(2026, 5, 1, 15, 0, 0);
  const twoMin = new Date(2026, 5, 1, 14, 58, 0);
  const rtf = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
  assert.equal(shortTime(twoMin.toISOString(), now), rtf.format(-2, "minute"));
});

test("shortTime uses clock time at 15 minutes", () => {
  const now = new Date(2026, 5, 1, 15, 0, 0);
  const fifteen = new Date(2026, 5, 1, 14, 45, 0);
  const rtf = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
  assert.notEqual(
    shortTime(fifteen.toISOString(), now),
    rtf.format(-15, "minute"),
  );
});

test("shortTime follows a later now", () => {
  const ts = new Date(2026, 5, 1, 14, 58, 0).toISOString();
  const at2 = new Date(2026, 5, 1, 15, 0, 0);
  const at5 = new Date(2026, 5, 1, 15, 3, 0);
  const rtf = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
  assert.equal(shortTime(ts, at2), rtf.format(-2, "minute"));
  assert.equal(shortTime(ts, at5), rtf.format(-5, "minute"));
});
