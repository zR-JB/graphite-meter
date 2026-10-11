// Payload in a lane or session worker: a download's bytes are counted, never touched, and reported at most every
// 50 ms; an upload sends one incompressible block over and over.

const READ_BYTES = 1024 * 1024;
const REPORT_MS = 50;
const BLOCK_BYTES = 4 * 1024 * 1024;
/** crypto.getRandomValues fills at most this much per call. */
const RANDOM_QUOTA = 65_536;

/** Counts `body` through one reused 1 MiB buffer: a default reader allocates per chunk, which halves what a worker
    reads at multi-Gbit/s. A stream without byte support is read chunk by chunk. */
export async function countBytes(body: ReadableStream<Uint8Array>, count: (bytes: number) => void): Promise<void> {
  let byob: ReadableStreamBYOBReader;
  try {
    byob = body.getReader({ mode: "byob" });
  } catch {
    const reader = body.getReader();
    try {
      for (let read = await reader.read(); !read.done; read = await reader.read()) count(read.value.byteLength);
    } finally {
      reader.releaseLock();
    }
    return;
  }
  let buffer = new ArrayBuffer(READ_BYTES);
  try {
    for (;;) {
      const { value, done } = await byob.read(new Uint8Array(buffer));
      if (done) return;
      count(value.byteLength);
      // A read detaches the buffer it fills and hands its memory back in the view.
      buffer = value.buffer;
    }
  } finally {
    byob.releaseLock();
  }
}

/** A download's counted bytes, reported at most every 50 ms with the measurement epoch they belong to. */
export class ByteReports {
  #bytes = 0;
  #since = performance.now();
  #epoch = 0;

  constructor(readonly report: (bytes: number, epoch: number) => void) {}

  /** Bound for read loops; a datagram loop passes the clock reading it already made. */
  count = (bytes: number, now = performance.now()): void => {
    this.#bytes += bytes;
    if (now - this.#since >= REPORT_MS) this.flush(now);
  };

  flush(now = performance.now()): void {
    if (this.#bytes) this.report(this.#bytes, this.#epoch);
    this.#bytes = 0;
    this.#since = now;
  }

  /** Bytes not yet reported belong to the previous epoch. */
  measure(epoch: number): void {
    this.#epoch = epoch;
    this.#bytes = 0;
  }
}

let block: Uint8Array<ArrayBuffer> | undefined;

/** 4 MiB of CSPRNG bytes, filled once per worker, so a compressing hop cannot flatter an upload. */
export function randomBlock(): Uint8Array<ArrayBuffer> {
  if (block) return block;
  block = new Uint8Array(BLOCK_BYTES);
  for (let at = 0; at < BLOCK_BYTES; at += RANDOM_QUOTA) crypto.getRandomValues(block.subarray(at, at + RANDOM_QUOTA));
  return block;
}
