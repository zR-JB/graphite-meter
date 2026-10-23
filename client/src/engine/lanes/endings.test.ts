import { expect, test } from "bun:test";
import { rows } from "../../protocol/pins.testutil";
import type { Ending } from "../types";
import { Backoff, REVOKED, refusal, sessionClosed } from "./endings";

test("every pinned upload refusal ends with its pinned reason, by code over HTTP and on a stream", async () => {
  const reasons = Object.fromEntries(await rows("uploadrefusalreasons.txt"));
  const refusals = await rows("uploadrefusals.txt");
  expect(refusals.map(([name]) => name).toSorted()).toEqual(Object.keys(reasons).toSorted());
  for (const [name, detail, status] of refusals as [string, string, string][]) {
    const reason = reasons[name];
    const expected = {
      reason,
      retry: reason === "server-busy" || reason === "timeout",
      // Only an id the server no longer knows may be replaced.
      rotate: name === "invalid" || undefined,
      // The idle bound is the server's own, so its reopen spends none of the stage's.
      bound: name === "idle" || undefined,
      detail,
    };
    expect(refusal(Number(status), name, detail), name).toEqual(expected);
    expect(refusal(null, name, detail), name).toEqual(expected);
  }
});

test("without a known code the status decides, and a stream refusal without one is unexpected", () => {
  const reason = (status: number | null, code?: string) => refusal(status, code, "").reason;
  // A bare 401 is a proxy's, never this server's ended login.
  expect([401, 429, 503, 0, 408, 500, 502, 400, 403, 409].map((status) => reason(status))).toEqual([
    "protocol-error",
    "server-busy",
    "server-busy",
    "connection-lost",
    "connection-lost",
    "connection-lost",
    "connection-lost",
    "protocol-error",
    "protocol-error",
    "protocol-error",
  ]);
  expect(reason(null)).toBe("protocol-error");
  expect(reason(503, "constructor")).toBe("server-busy");
});

test("the session endings the browser acts on match the lane ending pin", async () => {
  const endings = Object.fromEntries(
    (await rows("laneendings.txt")).map(([name, socket, session, reason]) => [
      name,
      { socket: Number(socket), session: Number(session), reason },
    ]),
  );
  expect(REVOKED).toEqual(endings.revoked!);
  expect(sessionClosed(endings.revoked!.session)?.reason).toBe("sign-in-required");
  for (const bound of [endings.idle!, endings.lifetime!])
    expect(sessionClosed(bound.session)).toMatchObject({ reason: "timeout", retry: true });
  for (const other of [endings.finished!, endings.shutdown!])
    expect(sessionClosed(other.session)).toBeNull();
});

test("a restart pauses 300 ms, a Retry-After raises it within 1.2 s, and a busy refusal holds until delivery", () => {
  const busy: Ending = { reason: "server-busy", retry: true, detail: "" };
  const lost: Ending = { reason: "connection-lost", retry: true, detail: "" };
  const backoff = new Backoff();
  expect([busy, busy].map((ending) => backoff.after(ending))).toEqual([300, 300]);
  expect(backoff.busy).toBe(true);
  backoff.reset();
  expect(backoff.busy).toBe(false);
  expect(backoff.after(lost)).toBe(300);
  expect(backoff.busy).toBe(false);
  expect(backoff.after({ ...busy, retryAfterMs: 1_000 })).toBe(1_000);
  expect(backoff.after({ ...busy, retryAfterMs: 5_000 })).toBe(1_200);
});
