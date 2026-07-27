// The idle keepalive's readiness wait. Its slot is shared: validateConnections
// aborts a probe and starts the next one without awaiting it, so two waits can
// exist over one worker.
import { test, expect } from "bun:test";
import { IdleKeepalive } from "./latencyChannel";
import type { CoreHost } from "../core";
import type { LatencyTarget } from "../../api/endpoints";

class FakeWorker {
  onmessage: ((event: MessageEvent) => void) | null = null;
  onerror: ((event: ErrorEvent) => void) | null = null;
  static last: FakeWorker | null = null;

  constructor() {
    FakeWorker.last = this;
  }
  postMessage(): void {}
  terminate(): void {}
  emit(data: unknown): void {
    this.onmessage?.({ data } as MessageEvent);
  }
}

const target: LatencyTarget = {
  id: "http://meter.test:7246",
  origin: "http://meter.test:7246",
  transport: "websocket",
  protocol: "http1",
  tls: false,
  routes: { probe: "/probe", ping: "/ws/ping" },
};

// The older wait settles itself, but the slot it settles from belongs to the
// newer one: clearing it drops the ready message the channel is about to send,
// and the newer wait then times out on a bus that is already up.
test("a superseded readiness wait does not silence the newer one", async () => {
  const realWorker = globalThis.Worker;
  globalThis.Worker = FakeWorker as unknown as typeof Worker;
  try {
    const keepalive = new IdleKeepalive({
      host: () => ({ emit() {} }) as unknown as CoreHost,
      throughputTarget: () => null,
      latencyTarget: () => target,
    });
    const abort = new AbortController();
    const superseded = keepalive.verifyReady(abort.signal);
    let ready = false;
    const current = keepalive.verifyReady().then(() => (ready = true));

    abort.abort();
    await expect(superseded).rejects.toThrow(/aborted/);

    FakeWorker.last!.emit({ type: "ready" });
    for (let turn = 0; turn < 10 && !ready; turn++) await Promise.resolve();
    expect(ready).toBe(true);
    await current;
    keepalive.stop();
  } finally {
    globalThis.Worker = realWorker;
  }
});
