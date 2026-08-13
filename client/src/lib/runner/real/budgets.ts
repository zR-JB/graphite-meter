// One set of budgets for every transport. A stage that cannot carry bytes has
// to be skipped in the same time whichever mechanism was selected, which is
// what makes a failure feel the same wherever it happens.

/** A path that has not answered by now is not going to. Applies to a QUIC
 *  handshake, a WebSocket upgrade and a progress feed alike. */
export const ESTABLISH_BUDGET_MS = 3000;

/** Margin over the establish budget for worker spawn and messaging, so an
 *  owner's deadline never fires before the worker's own. */
export const ESTABLISH_MARGIN_MS = 500;

/** Pause before reopening a dropped lane, so a failing one cannot spin. */
export const LANE_RESTART_BACKOFF_MS = 300;

/** A measured direction silent this long stalls independently. A sibling's
 * bytes cannot prove it healthy; a later positive byte resumes it cleanly. */
export const DIRECTION_PROGRESS_WINDOW_MS = 1500;

/** Absolute restart bound for a lane that keeps dropping after it once ran. */
export const LANE_MAX_RESTARTS = 40;

/** Time a graceful stop is given to finalize and acknowledge. */
export const STOP_GRACE_MS = 2500;

/** An h3 first hop can negotiate down on the first request, so the throughput
 *  probe retries. Bounded by attempts and by wall time together, so a path that
 *  never answers cannot hold the run open. The deadline also aborts the
 *  requests themselves. */
export const H3_PROBE_ATTEMPTS = 3;
export const H3_PROBE_DEADLINE_MS = 2000;

/** Grace for the server's terminal progress record once an upload is finalized,
 *  by the session worker's DELETE or the fetch feed's BYE alike. */
export const PROGRESS_FINAL_GRACE_MS = 1000;
