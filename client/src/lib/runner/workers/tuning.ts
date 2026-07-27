// The measurement constants both reading workers share. Everything only one
// worker reads lives in that worker; these two are read by the fetch download
// lane and the WebTransport session lane alike, and a reader that batched its
// reports differently from the other would not be comparable.

/** Reused BYOB read buffer, one per worker for fetch and per lane for a session.
 *  Reusing it is the only backpressure lever a reader has: without it Firefox
 *  reads far ahead into its own buffers, inflating RAM and undercounting. */
export const READ_BUF_BYTES = 1024 * 1024;

/** Byte deltas are batched to this cadence before crossing the thread. */
export const REPORT_GAP_MS = 50;
