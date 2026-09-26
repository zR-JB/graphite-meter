/* Reused BYOB read buffer, one per worker for fetch and per lane for a session. */
const READ_BUF_BYTES = 1024 * 1024;
/** Byte deltas are batched to this cadence before crossing the thread. */
export const REPORT_GAP_MS = 50;

export interface ProgressDelta {
  bytes: number;
  elapsedMs: number;
}

/** Owns the byte/time window shared by fetch and WebTransport downloads. */
interface ProgressWindow {
  reset(now?: number): void;
  add(bytes: number, now?: number): ProgressDelta | null;
  flush(now?: number): ProgressDelta | null;
}

export function progressWindow(
  now = performance.now(),
  gapMs = REPORT_GAP_MS,
): ProgressWindow {
  let bytes = 0;
  let startedAt = now;

  const reset = (at = performance.now()): void => {
    bytes = 0;
    startedAt = at;
  };
  const flush = (at = performance.now()): ProgressDelta | null => {
    const delta = { bytes, elapsedMs: at - startedAt };
    reset(at);
    return delta.bytes > 0 && delta.elapsedMs > 0 ? delta : null;
  };
  return {
    reset,
    add(amount, at = performance.now()) {
      bytes += amount;
      return at - startedAt < gapMs ? null : flush(at);
    },
    flush,
  };
}

/* The reused BYOB buffer is the read-side ceiling at multi-Gbit/s: a default reader allocates per chunk. */
export async function readBytes(
  body: ReadableStream<Uint8Array>,
  count: (n: number) => void,
): Promise<void> {
  let byob: ReadableStreamBYOBReader | null = null;
  try {
    byob = body.getReader({ mode: "byob" });
  } catch {
    // Not a byte stream: read its chunks instead.
  }
  if (byob) {
    let buf = new ArrayBuffer(READ_BUF_BYTES);
    for (;;) {
      const chunk = await byob.read(new Uint8Array(buf));
      if (chunk.done) return;
      if (chunk.value.byteLength) count(chunk.value.byteLength);
      // read() detaches the buffer and hands back the same backing store.
      buf = chunk.value.buffer as ArrayBuffer;
    }
  }
  const reader = body.getReader();
  for (;;) {
    const { value, done } = await reader.read();
    if (done) return;
    if (value) count(value.byteLength);
  }
}
