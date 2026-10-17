// Carries the payload of a lane or session worker. A download's bytes are counted, never touched, and reported in
// batches. An upload sends one incompressible block over and over.

const READ_BYTES = 1024 * 1024;
/** A server's download reports, all its lanes together, come about this often. A boundary then misses at most this
    much of one reporter's bytes, whatever the lane count, and the page handles about 100 reports a second. */
const REPORT_MS = 10;
const MIB = 1024 * 1024;
const BLOCK_BYTES = 4 * MIB;
/** crypto.getRandomValues fills at most this much per call. */
const RANDOM_QUOTA = 65_536;
/** A flush can look back over this many readings of a download's running total, well past the 250 ms it waits. */
const TOTALS = 512;

/** Counts `body` through one reused 1 MiB buffer. A default reader allocates per chunk, which halves what a worker
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

/** Reports a download's counted bytes in batches with the measurement epoch they belong to. Recent readings of the
    epoch's running total let a flush answer what was counted by a moment shortly back. */
export class ByteReports {
  #bytes = 0;
  #since = performance.now();
  #epoch = 0;
  #reportMs = REPORT_MS;
  #total = 0;
  readonly #times = new Float64Array(TOTALS);
  readonly #totals = new Float64Array(TOTALS);
  /** Readings taken this epoch. The latest is at `(#taken - 1) % TOTALS`. */
  #taken = 0;
  /** The latest count's time. */
  #last = Number.NEGATIVE_INFINITY;

  constructor(readonly report: (bytes: number, epoch: number) => void) {}

  /** Takes a share of the server's report rate as one of `reporters`. */
  share(reporters: number): void {
    this.#reportMs = REPORT_MS * Math.max(1, reporters);
  }

  /** Stays bound for read loops. Each count takes its own clock reading, or one a datagram loop already made. */
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

  /** Bytes not yet reported belong to the previous epoch. A lane reopening in its epoch keeps the epoch's total. */
  measure(epoch: number): void {
    this.#bytes = 0;
    if (epoch === this.#epoch) return;
    this.#epoch = epoch;
    this.#total = 0;
    this.#taken = 0;
    this.#last = Number.NEGATIVE_INFINITY;
  }

  /** Totals the epoch's counts by `at`, each at its own clock reading. Before the latest count, the latest reading at
      or before `at` answers. Past what is kept, the earliest kept reading answers with its own time. */
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

  /** Keeps a reading at most every millisecond, and at every report. */
  #keep(now: number, reported = false): void {
    if (!reported && this.#taken && now - this.#times[(this.#taken - 1) % TOTALS]! < 1) return;
    const slot = this.#taken++ % TOTALS;
    this.#times[slot] = now;
    this.#totals[slot] = this.#total;
  }
}

/** Makes 4 MiB of CSPRNG bytes, so a compressing hop cannot flatter an upload. Only the sender keeps them. */
export function randomBlock(): Uint8Array<ArrayBuffer> {
  const block = new Uint8Array(BLOCK_BYTES);
  for (let at = 0; at < BLOCK_BYTES; at += RANDOM_QUOTA) crypto.getRandomValues(block.subarray(at, at + RANDOM_QUOTA));
  return block;
}

/** Bodies repeat one random block, so memory stays at the block whatever their size. */
export const MAX_POST_BYTES = 256 * MIB;

/** Gives a lane its share of what a server's `streams` lanes post at once, by device memory. Where the browser does
    not say (Firefox), the pool spans all `sharing` lanes of the run, since Firefox stalls a body without an error when
    its POSTs together grow larger. */
export function largestPost(streams: number, sharing: number, deviceMemory: number | undefined): number {
  const [pool, lanes] =
    deviceMemory === undefined
      ? [128 * MIB, sharing]
      : [deviceMemory <= 2 ? 16 * MIB : deviceMemory <= 4 ? 24 * MIB : MAX_POST_BYTES, streams];
  return Math.max(2 * MIB, Math.floor(pool / Math.max(1, lanes)));
}
