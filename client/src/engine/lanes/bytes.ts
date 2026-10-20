// Lane payloads: downloads are counted, never touched; uploads send one incompressible block over and over.

const READ_BYTES = 1024 * 1024;
/** A server's lanes report together about this often: the page handles 100 a second, and exact counts come from flushes. */
const REPORT_MS = 10;
const MIB = 1024 * 1024;
const BLOCK_BYTES = 4 * MIB;
/** crypto.getRandomValues fills at most this much per call. */
const RANDOM_QUOTA = 65_536;
/** Readings of a download's running total a flush can look back over: well past the 250 ms a flush waits. */
const TOTALS = 512;

/** Counts `body` through one reused 1 MiB buffer: a default reader allocates per chunk, halving a worker's multi-Gbit/s reads. */
export async function countBytes(
  body: ReadableStream<Uint8Array>,
  count: (bytes: number) => void,
): Promise<void> {
  let byob: ReadableStreamBYOBReader;
  try {
    byob = body.getReader({ mode: "byob" });
  } catch {
    const reader = body.getReader();
    try {
      for (let read = await reader.read(); !read.done; read = await reader.read())
        count(read.value.byteLength);
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

/** A download's counted bytes, reported in batches by epoch; recent running totals let a flush look a moment back. */
export class ByteReports {
  #bytes = 0;
  #since = performance.now();
  #epoch = 0;
  #reportMs = REPORT_MS;
  #total = 0;
  readonly #times = new Float64Array(TOTALS);
  readonly #totals = new Float64Array(TOTALS);
  /** Readings taken; the latest is at `(#taken - 1) % TOTALS`. */
  #taken = 0;
  #last = Number.NEGATIVE_INFINITY;

  constructor(readonly report: (bytes: number, epoch: number) => void) {}

  /** One of `reporters` that share their server's report rate. */
  share(reporters: number): void {
    this.#reportMs = REPORT_MS * Math.max(1, reporters);
  }

  /** Bound for read loops, each count at its own clock reading; a datagram loop passes the one it already made. */
  count = (bytes: number, now = performance.now()): void => {
    this.#bytes += bytes;
    this.#total += bytes;
    this.#last = now;
    if (now - this.#since >= this.#reportMs) this.flush(now);
    else this.#keep(now);
  };

  flush(now = performance.now()): void {
    if (this.#bytes) this.report(this.#bytes, this.#epoch);
    this.#bytes = 0;
    this.#since = now;
    this.#keep(now, true);
  }

  /** A new epoch counts from `at`, though asked later; a lane reopening in its epoch keeps the epoch's total. */
  measure(epoch: number, at = performance.now()): void {
    this.#bytes = 0;
    if (epoch === this.#epoch) return;
    this.#epoch = epoch;
    const before = this.totalAt(at).total;
    for (let slot = 0; slot < Math.min(this.#taken, TOTALS); slot++)
      this.#totals[slot] = this.#totals[slot]! - before;
    this.#total -= before;
    this.#bytes = this.#total;
  }

  /** The epoch's total as counted by `at`; past what is kept, the earliest reading kept and its time. */
  totalAt(at: number): { total: number; at: number } {
    if (at >= this.#last) return { total: this.#total, at };
    const kept = Math.min(this.#taken, TOTALS);
    for (let back = 1; back <= kept; back++) {
      const slot = (this.#taken - back) % TOTALS;
      if (this.#times[slot]! <= at) return { total: this.#totals[slot]!, at };
    }
    if (this.#taken <= TOTALS) return { total: 0, at };
    const oldest = this.#taken % TOTALS;
    return { total: this.#totals[oldest]!, at: this.#times[oldest]! };
  }

  flushed(at: number) {
    this.flush();
    const counted = this.totalAt(at - performance.timeOrigin);
    return {
      type: "flushed" as const,
      total: counted.total,
      at: performance.timeOrigin + counted.at,
    };
  }

  /** A reading at most every millisecond, and at every report. */
  #keep(now: number, reported = false): void {
    if (!reported && this.#taken && now - this.#times[(this.#taken - 1) % TOTALS]! < 1) return;
    const slot = this.#taken++ % TOTALS;
    this.#times[slot] = now;
    this.#totals[slot] = this.#total;
  }
}

/** 4 MiB of CSPRNG bytes, so a compressing hop cannot flatter an upload; kept only by whoever is sending them. */
export function randomBlock(): Uint8Array<ArrayBuffer> {
  const block = new Uint8Array(BLOCK_BYTES);
  for (let at = 0; at < BLOCK_BYTES; at += RANDOM_QUOTA)
    crypto.getRandomValues(block.subarray(at, at + RANDOM_QUOTA));
  return block;
}

/** Bodies repeat one random block by reference; only Firefox's file-backed copy holds their full size. */
export const MAX_POST_BYTES = 256 * MIB;

/** A lane's share of a server's posting budget by device memory; Firefox reports none and stalls larger POSTs, so it pools all lanes. */
export function largestPost(
  streams: number,
  sharing: number,
  deviceMemory: number | undefined,
): number {
  const [pool, lanes] =
    deviceMemory === undefined
      ? [128 * MIB, sharing]
      : [deviceMemory <= 2 ? 16 * MIB : deviceMemory <= 4 ? 24 * MIB : MAX_POST_BYTES, streams];
  return Math.max(2 * MIB, Math.floor(pool / Math.max(1, lanes)));
}
