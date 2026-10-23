import { expect, test } from "bun:test";
import { rows } from "../../protocol/pins.testutil";
import type { Ending, FailureReason } from "../types";
import { downloadRefused, REVOKED, refusal, restartMs, sessionClosed } from "./endings";

test("every pinned refusal ends with its pinned reason, by code over HTTP and on a stream", async () => {
  const refusals = (await rows("refusals.txt")) as [string, string, string, FailureReason][];
  for (const [code, message, status, reason] of refusals) {
    const retry = ["client-limit", "server-busy", "timeout"].includes(reason);
    const expected = {
      reason,
      retry,
      // Only an id the server no longer knows may be replaced.
      rotate: code === "invalid" || undefined,
      // The idle bound is the server's own, so its reopen spends none of the stage's.
      bound: code === "idle" || undefined,
      detail: message,
    };
    expect(refusal(Number(status), code, message), code).toEqual(expected);
    expect(refusal(null, code, message), code).toEqual(expected);
    const answer = new Response(message, {
      status: Number(status),
      headers: { "Graphite-Meter-Refusal": code },
    });
    expect(downloadRefused(answer).reason, code).toBe(reason);
  }
});

test("without a known code the status decides, and a stream refusal without one is unexpected", () => {
  const reason = (status: number | null, code?: string) => refusal(status, code, "").reason;
  // A bare 401 is a proxy's, never this server's ended login.
  expect([401, 429, 503, 0, 408, 500, 502, 400, 403, 409].map((status) => reason(status))).toEqual([
    "protocol-error",
    "client-limit",
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
  expect(downloadRefused(new Response(null, { status: 502 })).reason).toBe("protocol-error");
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

test("a restart pauses 300 ms, and a Retry-After raises it within 1.2 s", () => {
  const limited: Ending = { reason: "client-limit", retry: true, detail: "" };
  expect(restartMs(limited)).toBe(300);
  expect(restartMs({ ...limited, retryAfterMs: 1_000 })).toBe(1_000);
  expect(restartMs({ ...limited, retryAfterMs: 5_000 })).toBe(1_200);
});
