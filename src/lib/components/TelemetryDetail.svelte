<script lang="ts">
  /* ============================================================
   * <TelemetryDetail> — right inspector, below (§3.7)
   * Jitter distribution (canvas — deferred to the canvas stage),
   * bufferbloat idle-vs-loaded bars + grade chip, percentile table.
   * ============================================================ */
  import { console as store } from "../state/console.svelte";
  import { fmtMs } from "../format";
  import { ICON } from "../constants";

  const bb = $derived(store.result?.bufferbloat ?? null);
  const lat = $derived(store.result?.latency ?? null);

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
    <!-- Jitter distribution (canvas placeholder until the canvas stage) -->
    <div class="block">
      <span class="block-title">Jitter distribution</span>
      <div class="hist-ph">SparkEngine histogram</div>
    </div>

    <!-- Bufferbloat -->
    <div class="block">
      <div class="block-row">
        <span class="block-title">Bufferbloat</span>
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
            <th>p50</th>
            <td>{lat ? `${fmtMs(lat.p50Ms)}` : dash}</td>
          </tr>
          <tr>
            <th>p95</th>
            <td>{lat ? `${fmtMs(lat.p95Ms)}` : dash}</td>
            <th>loss</th>
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
    border-radius: var(--radius-lg);
    background: var(--surface-1);
    box-shadow: var(--shadow-card);
    overflow: clip;
  }
  .card-head {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 14px;
    border-bottom: 1px solid var(--border);
    background: linear-gradient(180deg, var(--surface-2), transparent);
  }
  .head-ico {
    display: grid;
    place-items: center;
    width: 34px;
    height: 34px;
    border-radius: var(--radius-md);
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
