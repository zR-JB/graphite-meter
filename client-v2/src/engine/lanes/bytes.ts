// Payload in a lane or session worker: a download's bytes are counted, never touched, and reported in batches; an
// upload sends one incompressible block over and over.

const READ_BYTES = 1024 * 1024;
/** A server's download reports, all its lanes together, come about this often: a boundary then misses at most this
    much of one reporter's bytes, whatever the lane count, and the page handles about 100 reports a second. */
const REPORT_MS = 10;
const MIB = 1024 * 1024;
const BLOCK_BYTES = 4 * MIB;
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

/** A download's counted bytes, reported at most every 10 ms per reporter of its server with the measurement epoch they
    belong to. */
export class ByteReports {
  #bytes = 0;
  #since = performance.now();
  #epoch = 0;
  #reportMs = REPORT_MS;

  constructor(readonly report: (bytes: number, epoch: number) => void) {}

  /** One of `reporters` that share their server's report rate. */
  share(reporters: number): void {
    this.#reportMs = REPORT_MS * Math.max(1, reporters);
  }

  /** Bound for read loops; a datagram loop passes the clock reading it already made. */
  count = (bytes: number, now = performance.now()): void => {
    this.#bytes += bytes;
    if (now - this.#since >= this.#reportMs) this.flush(now);
  };

  /** Counts without reading the clock; the next count or flush reports the bytes. */
  add(bytes: number): void {
    this.#bytes += bytes;
  }

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

/** 4 MiB of CSPRNG bytes, so a compressing hop cannot flatter an upload; kept only by whoever is sending them. */
export function randomBlock(): Uint8Array<ArrayBuffer> {
  const block = new Uint8Array(BLOCK_BYTES);
  for (let at = 0; at < BLOCK_BYTES; at += RANDOM_QUOTA) crypto.getRandomValues(block.subarray(at, at + RANDOM_QUOTA));
  return block;
}

/** Bodies repeat one random block, so memory stays at the block whatever their size. */
export const MAX_POST_BYTES = 256 * MIB;

/** A lane's share of what a server's lanes post at once, as v1 sizes it: 16 MiB on a device with 2 GB or less, 24 MiB
    up to 4 GB, else 256 MiB; 128 MiB where the browser does not say (Firefox, which can stall a larger body without
    an error). Never under 2 MiB a lane. */
export function largestPost(streams: number, deviceMemory: number | undefined): number {
  const pool =
    deviceMemory === undefined
      ? 128 * MIB
      : deviceMemory <= 2
        ? 16 * MIB
        : deviceMemory <= 4
          ? 24 * MIB
          : MAX_POST_BYTES;
  return Math.max(2 * MIB, Math.floor(pool / Math.max(1, streams)));
}
