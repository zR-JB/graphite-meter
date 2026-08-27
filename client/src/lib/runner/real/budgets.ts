// A stage that cannot carry bytes has to be skipped in the same time whichever mechanism was selected, which is what.

/* A path that has not answered by now is not going to. */
export const ESTABLISH_BUDGET_MS = 3000;

/* Margin over the establish budget for worker spawn and messaging, so an owner's deadline never fires before the. */
export const ESTABLISH_MARGIN_MS = 500;

/** Pause before reopening a dropped lane, so a failing one cannot spin. */
export const LANE_RESTART_BACKOFF_MS = 300;

/* A measured direction silent this long stalls independently. */
export const DIRECTION_PROGRESS_WINDOW_MS = 1500;

/** Time a graceful stop is given to finalize and acknowledge. */
export const STOP_GRACE_MS = 2500;

/* Bounded by attempts and by wall time together, so a path that never answers cannot hold the run open. */
export const H3_PROBE_ATTEMPTS = 3;
export const H3_PROBE_DEADLINE_MS = 2000;

/* Grace for the server's terminal progress record once an upload is finalized, by the session worker's DELETE or. */
export const PROGRESS_FINAL_GRACE_MS = 1000;
