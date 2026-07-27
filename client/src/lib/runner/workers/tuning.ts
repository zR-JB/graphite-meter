// Every measurement constant a transfer worker reads, in one table. The shipped
// values are the defaults; a benchmark varies one without touching the code.

export type ReaderMode = "byob" | "default";
export type UploadBody = "blob" | "arrayBuffer" | "stream";
export type UploadDrain = "arrayBuffer" | "cancel";
export type CongestionControl = "default" | "throughput" | "low-latency";

export interface Tuning {
  /** Reused BYOB read buffer, one per worker for fetch and per lane for a session. */
  readBufBytes: number;
  /** "default" forces the allocating reader, pricing BYOB against it. */
  reader: ReaderMode;
  /** Byte deltas are batched to this cadence before crossing the thread. */
  reportGapMs: number;
  /** Upload reservoir, divided across the lanes and also the sizer's ceiling.
   *  Worth +10.9% at 256 MiB over 64 MiB; see docs/BENCHMARKS.md. */
  uploadTotalPoolBytes: number;
  /** Wall time each POST aims to span. */
  targetPostMs: number;
  /** Smallest POST, below which per-request overhead dominates. */
  minPostBytes: number;
  /** How the POST body reaches fetch. A Blob slice is a view; an ArrayBuffer
   *  copies; "stream" sends one endless request body and needs h2 or h3. */
  uploadBody: UploadBody;
  /** How the response is released so keep-alive serves the next POST. */
  uploadDrain: UploadDrain;
  /** Bytes per WebTransport stream write. */
  writeChunkBytes: number;
  /** Session congestion control hint. */
  congestionControl: CongestionControl;
  /** Read the clock every Nth datagram rather than every one. */
  datagramClockEvery: number;
  /** Pins navigator.deviceMemory, which only Chromium reports. */
  deviceMemory?: number;
}

export const DEFAULT_TUNING: Tuning = {
  readBufBytes: 1024 * 1024,
  reader: "byob",
  reportGapMs: 50,
  uploadTotalPoolBytes: 256 * 1024 * 1024,
  targetPostMs: 500,
  minPostBytes: 128 * 1024,
  uploadBody: "blob",
  uploadDrain: "arrayBuffer",
  writeChunkBytes: 4 * 1024 * 1024,
  congestionControl: "throughput",
  datagramClockEvery: 1,
};

export function tuned(tune?: Partial<Tuning>): Tuning {
  return tune ? { ...DEFAULT_TUNING, ...tune } : DEFAULT_TUNING;
}
