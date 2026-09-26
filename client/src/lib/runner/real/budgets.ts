// One set of budgets for every transport, so a failure takes the same time wherever it happens.

/* A path that has not answered by now is not going to. */
export const ESTABLISH_BUDGET_MS = 3000;

/* Margin for worker spawn and messaging, so an owner's deadline never fires before the worker's own. */
export const ESTABLISH_MARGIN_MS = 500;

/** Pause before reopening a dropped lane, so a failing one cannot spin. */
export const LANE_RESTART_BACKOFF_MS = 300;

/* Measured evidence silent this long stalls a direction; a server that stays silent leaves the stage. */
export const DIRECTION_PROGRESS_WINDOW_MS = 1500;

/** Time a graceful stop is given to finalize and acknowledge. */
export const STOP_GRACE_MS = 2500;

/* Bounded by attempts and by wall time together, so a path that never answers cannot hold the run open. */
export const H3_PROBE_ATTEMPTS = 8;
export const H3_PROBE_DEADLINE_MS = 2000;

/* Grace for the server's terminal progress record once an upload is finalized. */
export const PROGRESS_FINAL_GRACE_MS = 1000;
