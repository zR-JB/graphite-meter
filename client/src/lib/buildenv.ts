/* ============================================================
 * Build-time client configuration — frozen at compile time by
 * Vite `define` (see ../../vite.config.ts). Driven by the
 * GM_CLIENT_* env vars set by `just prod` / `docker build`.
 *
 * These are literal substitutions, so branches gated on the raw
 * tokens constant-fold: a build with GM_CLIENT_ALLOW_DUMMY=0
 * tree-shakes the DummyBackend (and the simulation UI) out of the
 * bundle entirely. Read `BUILD.*` for plain values; for the
 * tree-shaking-critical branches, reference the raw `__GM_*__`
 * token directly so Rollup is guaranteed to fold it.
 * ============================================================ */

export const BUILD = {
  /** Default sample source when more than one is built in (allowDummy). */
  defaultEngine: __GM_DEFAULT_ENGINE__,
  /** Whether the synthetic DummyBackend (+ anomaly simulation) is included. */
  allowDummy: __GM_ALLOW_DUMMY__,
  /** Whether the Developer settings tab (debug logging + simulation) exists. */
  devTools: __GM_DEV_TOOLS__,
  /** Identity shown in the status bar (`build …`). */
  buildLabel: __GM_BUILD_LABEL__,
  /** Canonical client version, `<semver>+<label>` (e.g. "0.0.0+abc1234").
   *  Shown in the Endpoint info, sent on preflight, mirrored in version.json. */
  clientVersion: __GM_CLIENT_VERSION__,
} as const;
