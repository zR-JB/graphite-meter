<script lang="ts">
  /* ============================================================
   * <TelemetryDetail> — right inspector, below (§3.7)
   * Jitter distribution (canvas — deferred to the canvas stage),
   * bufferbloat idle-vs-loaded bars + grade chip, percentile table.
   * ============================================================ */
  import { store } from "../state/store.svelte";
  import { fmtMs } from "../format";
  import { ICON } from "../constants";
  import { tooltip, JARGON } from "../actions/tooltip";

  const bb = $derived(store.result?.bufferbloat ?? null);
  const lat = $derived(store.result?.latency ?? null);

  // Latency distribution (idle vs loaded) — a token-styled DOM-bar histogram
  // that recomputes only when the sample set changes (not per frame). Splits
  // non-lost pings into idle (!underLoad) and loaded (underLoad) series over a
  // shared domain + shared vertical scale so the two read comparably.
  const BINS = 16;
  const histo = $derived.by(() => {
    const samples = store.latency.filter((s) => !s.lost);
    if (samples.length < 2) return null;
    let min = Infinity;
    let max = -Infinity;
    for (const s of samples) {
      if (s.rttMs < min) min = s.rttMs;
      if (s.rttMs > max) max = s.rttMs;
    }
    const span = Math.max(1, max - min);
    const idle = new Array(BINS).fill(0);
    const loaded = new Array(BINS).fill(0);
    for (const s of samples) {
      const i = Math.min(
        BINS - 1,
        Math.max(0, Math.floor(((s.rttMs - min) / span) * BINS)),
      );
      if (s.underLoad) loaded[i]++;
      else idle[i]++;
    }
    const peak = Math.max(...idle, ...loaded, 1);
    return { idle, loaded, peak, min, max };
  });

  // Bar widths: scale idle/loaded against the larger of the two.
  const barScale = $derived(bb ? Math.max(bb.idleMs, bb.loadedMs, 1) : 1);

  function gradeTone(g: string): string {
    if (g === "A" || g === "B") return "ok";
    if (g === "C") return "warn";
    return "err";
  }

  const dash = "—";
</script>

<section class="card">
  <header class="card-head">
    <span class="head-ico">{@html ICON.activity}</span>
    <h3>Telemetry</h3>
  </header>

  <div class="body">
    <!-- Latency distribution — idle vs loaded RTT histogram (DOM bars). -->
    <div class="block">
      <div class="block-row">
        <span class="block-title term" use:tooltip={JARGON.jitter}>
          Latency distribution (idle / loaded)<span class="info-dot"
            >{@html ICON.info}</span
          >
        </span>
        <span class="hist-legend" aria-hidden="true">
          <span class="hist-key"><i class="sw sw-idle"></i>idle</span>
          <span class="hist-key"><i class="sw sw-loaded"></i>loaded</span>
        </span>
      </div>
      {#if histo}
        <div class="hist" role="img" aria-label="Latency distribution histogram">
          {#each histo.idle as _, i (i)}
            <div class="hist-bin">
              <span
                class="hist-bar hist-idle"
                style="height:{(histo.idle[i] / histo.peak) *
                  100}%;min-height:{histo.idle[i] ? 1 : 0}px"
              ></span>
              <span
                class="hist-bar hist-loaded"
                style="height:{(histo.loaded[i] / histo.peak) *
                  100}%;min-height:{histo.loaded[i] ? 1 : 0}px"
              ></span>
            </div>
          {/each}
        </div>
        <div class="hist-ticks">
          <span>{fmtMs(histo.min)}</span>
          <span>{fmtMs(histo.max)}</span>
        </div>
      {:else}
        <div class="hist-ph">Awaiting pings…</div>
      {/if}
    </div>

    <!-- Bufferbloat -->
    <div class="block">
      <div class="block-row">
        <span class="block-title term" use:tooltip={JARGON.bufferbloat}>
          Bufferbloat<span class="info-dot">{@html ICON.info}</span>
        </span>
        {#if bb}
          <span class="grade grade-{gradeTone(bb.grade)}">{bb.grade}</span>
        {:else}
          <span class="grade grade-idle">{dash}</span>
        {/if}
      </div>
      <div class="bb-bars">
        <div class="bb-line">
          <span class="bb-label">Idle</span>
          <span class="bb-track">
            <span
              class="bb-fill bb-idle"
              style="width:{bb ? (bb.idleMs / barScale) * 100 : 0}%"
            ></span>
          </span>
          <span class="bb-val">{bb ? `${fmtMs(bb.idleMs)}ms` : dash}</span>
        </div>
        <div class="bb-line">
          <span class="bb-label">Loaded</span>
          <span class="bb-track">
            <span
              class="bb-fill bb-loaded"
              style="width:{bb ? (bb.loadedMs / barScale) * 100 : 0}%"
            ></span>
          </span>
          <span class="bb-val">{bb ? `${fmtMs(bb.loadedMs)}ms` : dash}</span>
        </div>
      </div>
      <div class="bb-delta">
        Δ {bb ? `+${fmtMs(bb.increaseMs)} ms` : dash}
      </div>
    </div>

    <!-- Percentiles -->
    <div class="block">
      <span class="block-title">Percentiles</span>
      <table class="pct">
        <tbody>
          <tr>
            <th>min</th>
            <td>{lat ? `${fmtMs(lat.minMs)}` : dash}</td>
            <th class="term" use:tooltip={JARGON.p50}>p50</th>
            <td>{lat ? `${fmtMs(lat.p50Ms)}` : dash}</td>
          </tr>
          <tr>
            <th class="term" use:tooltip={JARGON.p95}>p95</th>
            <td>{lat ? `${fmtMs(lat.p95Ms)}` : dash}</td>
            <th class="term" use:tooltip={JARGON.packetLoss}>loss</th>
            <td>{lat ? `${lat.packetLossPct.toFixed(1)}%` : dash}</td>
          </tr>
        </tbody>
      </table>
    </div>
  </div>
</section>

<style>
  .card {
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-1);
    box-shadow: var(--elev-tile);
    overflow: clip;
  }
  .card-head {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-3);
    border-bottom: 1px solid var(--border);
    background: linear-gradient(180deg, var(--surface-2), transparent);
  }
  .head-ico {
    display: grid;
    place-items: center;
    width: 34px;
    height: 34px;
    border-radius: var(--r-well);
    border: 1px solid color-mix(in srgb, var(--signal) 34%, var(--border));
    background: var(--signal-soft);
    color: var(--signal);
  }
  .head-ico :global(svg) {
    width: 18px;
    height: 18px;
  }
  .card-head h3 {
    font-size: 13px;
    font-weight: 820;
    letter-spacing: -0.02em;
    margin: 0;
  }

  .body {
    padding: 14px;
    display: flex;
    flex-direction: column;
    gap: 18px;
  }
  .block-title {
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--text-soft);
  }

  /* Jargon term affordance (§14.3): a dotted underline + small info dot cues
     that the term carries a plain-language tooltip on hover/focus. */
  .term {
    cursor: help;
    text-decoration: underline dotted color-mix(in srgb, var(--text-soft) 70%, transparent);
    text-underline-offset: 3px;
  }
  .term.block-title {
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }
  .term:hover,
  .term:focus-visible {
    color: var(--text);
  }
  .term:focus-visible {
    outline: 2px solid color-mix(in srgb, var(--brand) 70%, transparent);
    outline-offset: 2px;
    border-radius: var(--radius-xs);
  }
  .info-dot {
    display: inline-grid;
    place-items: center;
    opacity: 0.6;
  }
  .info-dot :global(svg) {
    width: 12px;
    height: 12px;
  }
  .block-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 10px;
  }

  .hist-ph {
    margin-top: 8px;
    height: 64px;
    display: grid;
    place-items: center;
    border: 1px dashed var(--border-strong);
    border-radius: var(--radius-sm);
    color: var(--text-soft);
    font-family: var(--font-mono);
    font-size: 11px;
  }

  /* Idle-vs-loaded RTT histogram — 16 paired sub-bars, shared vertical scale. */
  .hist {
    margin-top: 8px;
    height: 64px;
    display: flex;
    align-items: flex-end;
    gap: 3px;
    padding: 0 1px;
    border-bottom: 1px solid var(--border);
  }
  .hist-bin {
    flex: 1;
    height: 100%;
    display: flex;
    align-items: flex-end;
    justify-content: center;
    gap: 1px;
    min-width: 0;
  }
  .hist-bar {
    flex: 1;
    min-width: 0;
    border-radius: var(--radius-xs) var(--radius-xs) 0 0;
    transition: height var(--dur-graph) var(--ease-out);
  }
  .hist-idle {
    background: var(--phase-latency);
  }
  .hist-loaded {
    background: var(--warn);
  }
  .hist-ticks {
    display: flex;
    justify-content: space-between;
    margin-top: 4px;
    font-family: var(--font-mono);
    font-size: 9px;
    color: var(--text-soft);
  }

  /* Legend — tiny swatches decoding the two series. */
  .hist-legend {
    display: flex;
    gap: 10px;
  }
  .hist-key {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: 9px;
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--text-soft);
  }
  .sw {
    width: 9px;
    height: 9px;
    border-radius: 2px;
  }
  .sw-idle {
    background: var(--phase-latency);
  }
  .sw-loaded {
    background: var(--warn);
  }

  /* Bufferbloat */
  .bb-bars {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .bb-line {
    display: grid;
    grid-template-columns: 48px 1fr 56px;
    align-items: center;
    gap: 8px;
  }
  .bb-label {
    font-size: 11px;
    color: var(--text-muted);
  }
  .bb-track {
    height: 8px;
    border-radius: var(--radius-xs);
    background: var(--surface-inset);
    overflow: hidden;
  }
  .bb-fill {
    display: block;
    height: 100%;
    border-radius: inherit;
    transition: width var(--dur-graph) var(--ease-out);
  }
  .bb-idle {
    background: var(--signal);
  }
  .bb-loaded {
    background: var(--warn);
  }
  .bb-val {
    text-align: right;
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--text);
  }
  .bb-delta {
    margin-top: 8px;
    text-align: right;
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--text-muted);
  }

  .grade {
    display: grid;
    place-items: center;
    min-width: 26px;
    height: 26px;
    padding: 0 6px;
    border-radius: var(--radius-sm);
    font-family: var(--font-mono);
    font-weight: 700;
    font-size: 13px;
  }
  .grade-ok {
    background: var(--ok-soft);
    color: var(--ok);
  }
  .grade-warn {
    background: var(--warn-soft);
    color: var(--warn);
  }
  .grade-err {
    background: var(--err-soft);
    color: var(--err);
  }
  .grade-idle {
    background: var(--surface-inset);
    color: var(--text-soft);
  }

  /* Percentiles */
  .pct {
    width: 100%;
    margin-top: 8px;
    border-collapse: collapse;
    font-family: var(--font-mono);
  }
  .pct th {
    text-align: left;
    font-weight: 500;
    font-size: 11px;
    color: var(--text-soft);
    padding: 4px 6px 4px 0;
  }
  .pct td {
    text-align: right;
    font-size: 12px;
    color: var(--text);
    padding: 4px 16px 4px 0;
  }
</style>
