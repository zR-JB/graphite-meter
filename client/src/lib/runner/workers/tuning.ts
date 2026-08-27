// The measurement constants both reading workers share.

/* Reused BYOB read buffer, one per worker for fetch and per lane for a session. */
export const READ_BUF_BYTES = 1024 * 1024;

/** Byte deltas are batched to this cadence before crossing the thread. */
export const REPORT_GAP_MS = 50;
