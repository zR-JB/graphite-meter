<script lang="ts">
  /* ============================================================
   * <LatencyProfile> — native latency distribution lanes (§13.5)
   * Ports linerate's SVG LatencyProfile to token-styled DOM — NO
   * <svg>. Three lanes (idle / loaded-down / loaded-up), each with
   * a min–max range bar, a P10–P90 band, an average marker, a
   * current marker, and a striped loss indicator + hover readout.
   * Stats come from the store's shared `latencyLanes` derived (which
   * uses the single `quantile()` in format.ts); the domain is the
   * shared `niceDomain()` so it scales like the result chart's axis.
   * ============================================================ */
  import { console as store, type LatencyLane, type StageKey } from "../state/console.svelte";
  import { fmtMs, niceDomain } from "../format";
  import { tooltip } from "../actions/tooltip";

  // Compact mode (embedded under the gauge): no card header, tighter rows, and
  // empty/not-yet-running lanes suppressed so warmup shows no dead rows.
  interface Props {
    compact?: boolean;
  }
  let { compact = false }: Props = $props();

  const PROFILE_HELP =
    "Latency profile: how steady your ping is, idle and under load. Each bar's shaded band is the P10–P90 range (your typical pings); the dot is the latest reading. Tighter is steadier.";

  type MetricKey = "min" | "p10" | "average" | "p90" | "max" | "current";

  const LANE_META: Record<StageKey, { label: string; tone: string }> = {
    latency: { label: "Idle", tone: "idle" },
    download: { label: "Loaded Down", tone: "download" },
    upload: { label: "Loaded Up", tone: "upload" },
  };
  const METRIC_LABELS: Record<MetricKey, string> = {
    min: "Min",
    p10: "P10",
    average: "Avg",
    p90: "P90",
    max: "Max",
    current: "Latest",
  };

  // Filter lanes by enabled stages; skip ones with no samples yet (waiting).
  // In compact mode additionally suppress lanes that have no data AND aren't
  // the currently-running phase, so the warmup/early state shows no dead rows
  // (full mode keeps every enabled lane so it can read as a stable legend).
  const lanes = $derived(
    store.latencyLanes.filter(
      (lane) =>
        store.config.stages[lane.key] &&
        (!compact || lane.average != null || lane.key === store.phase),
    ),
  );

  // Shared centered, snapped latency domain across every visible lane.
  const domain = $derived.by(() => {
    const values: number[] = [];
    for (const lane of lanes) {
      if (lane.min != null) values.push(lane.min);
      if (lane.max != null) values.push(lane.max);
    }
    return niceDomain(values, { floor: 20 });
  });

  const ticks = $derived([
    domain.min,
    domain.min + domain.span / 2,
    domain.max,
  ]);

  let hover = $state<{ key: StageKey; metric: MetricKey; side: "left" | "right" } | null>(null);

  const hoverLane = $derived(hover ? (lanes.find((l) => l.key === hover!.key) ?? null) : null);
  const hoverValue = $derived(
    hoverLane && hover ? metricValue(hoverLane, hover.metric) : null,
  );

  function pos(value: number | null): number {
    if (value == null) return 0;
    return Math.min(100, Math.max(0, ((value - domain.min) / domain.span) * 100));
  }
  function rangeWidth(min: number | null, max: number | null): number {
    if (min == null || max == null) return 0;
    return Math.max(1.5, pos(max) - pos(min));
  }
  function tickLabel(v: number): string {
    return v <= 0 ? "0" : fmtMs(v);
  }
  function lossLabel(ratio: number): string {
    if (ratio <= 0) return "";
    return `${(ratio * 100).toFixed(ratio < 0.01 ? 2 : 1)}% loss`;
  }
  function metricValue(lane: LatencyLane, metric: MetricKey): number | null {
    return lane[metric];
  }

  // Metrics present on a lane, used for hover nearest-snap.
  function entries(lane: LatencyLane): { metric: MetricKey; value: number }[] {
    return (Object.keys(METRIC_LABELS) as MetricKey[]).flatMap((metric) => {
      const value = metricValue(lane, metric);
      return value == null ? [] : [{ metric, value }];
    });
  }
  function nearestMetric(lane: LatencyLane, target: number): MetricKey | null {
    return entries(lane).reduce<MetricKey | null>((best, e) => {
      if (!best) return e.metric;
      const bv = metricValue(lane, best)!;
      return Math.abs(e.value - target) < Math.abs(bv - target) ? e.metric : best;
    }, null);
  }
  function hoverContext(lane: LatencyLane, metric: MetricKey): string {
    if (metric === "p10" || metric === "p90") {
      if (lane.p10 == null || lane.p90 == null) return "";
      return `P10–P90 ${fmtMs(lane.p10)} – ${fmtMs(lane.p90)}`;
    }
    if (metric === "current") {
      return lane.average == null ? "" : `Avg ${fmtMs(lane.average)}`;
    }
    if (metric === "average") {
      if (lane.min == null || lane.max == null) return "";
      return `Range ${fmtMs(lane.min)} – ${fmtMs(lane.max)}`;
    }
    return lane.average == null ? "" : `Avg ${fmtMs(lane.average)}`;
  }

  function onStripMove(e: PointerEvent, key: StageKey) {
    const lane = lanes.find((l) => l.key === key);
    if (!lane) return;
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const ratio = Math.min(1, Math.max(0, (e.clientX - rect.left) / rect.width));
    const metric = nearestMetric(lane, domain.min + ratio * domain.span);
    if (!metric) {
      hover = null;
      return;
    }
    hover = { key, metric, side: ratio > 0.55 ? "left" : "right" };
  }
  function clearHover() {
    hover = null;
  }
</script>

<section class="card" class:compact aria-label="Latency distribution">
  <header class="card-head">
    <h3 class="term" use:tooltip={PROFILE_HELP}>Latency Profile</h3>
    <p>Range / avg / loss</p>
  </header>
  {#if compact}
    <span class="compact-caption" use:tooltip={PROFILE_HELP}>Latency profile</span>
  {/if}

  <div class="lanes" role="img" aria-label="Latency, jitter and loss by phase">
    {#each lanes as lane (lane.key)}
      {@const meta = LANE_META[lane.key]}
      <div class="lane" data-tone={meta.tone} data-active={lane.active}>
        <div class="lane-meta">
          <span>{meta.label}</span>
          <strong>{lane.average == null ? "waiting" : `avg ${fmtMs(lane.average)}`}</strong>
          <em>
            {lane.min == null || lane.max == null
              ? "range —"
              : `${fmtMs(lane.min)} – ${fmtMs(lane.max)}`}
          </em>
        </div>

        <div class="strip">
          <div class="ticks" aria-hidden="true">
            {#each ticks as tick, i (`${lane.key}-${i}`)}
              <span style="left:{pos(tick)}%">{tickLabel(tick)}</span>
            {/each}
          </div>

          <div
            class="track"
            role="img"
            aria-label="{meta.label} latency profile"
            onpointermove={(e) => onStripMove(e, lane.key)}
            onpointerleave={clearHover}
          >
            {#if lane.min != null && lane.max != null}
              <span class="range" style="left:{pos(lane.min)}%;width:{rangeWidth(lane.min, lane.max)}%"></span>
            {/if}
            {#if lane.p10 != null && lane.p90 != null}
              <span class="band" style="left:{pos(lane.p10)}%;width:{rangeWidth(lane.p10, lane.p90)}%"></span>
            {/if}
            {#if lane.average != null}
              <i class="avg-marker" style="left:{pos(lane.average)}%"></i>
            {/if}
            {#if lane.current != null}
              <i class="cur-marker" title="Latest {fmtMs(lane.current)}" style="left:{pos(lane.current)}%"></i>
            {/if}
            {#if lane.lossRatio > 0}
              <i
                class="loss-marker"
                title={lossLabel(lane.lossRatio)}
                style="width:{Math.min(34, Math.max(8, lane.lossRatio * 100))}%"
              ></i>
            {/if}

            {#if hover?.key === lane.key && hoverValue != null}
              <span class="guide" style="left:{pos(hoverValue)}%"></span>
              <span class="pin" class:avg={hover.metric === "average"} style="left:{pos(hoverValue)}%"></span>
              <div class="hover-card" class:left={hover.side === "left"} style="left:{pos(hoverValue)}%">
                <div class="hc-head">
                  <span>{meta.label}</span>
                  <strong>{METRIC_LABELS[hover.metric]} {fmtMs(hoverValue)}</strong>
                </div>
                {#if hoverContext(lane, hover.metric)}
                  <p>{hoverContext(lane, hover.metric)}</p>
                {/if}
                {#if lane.lossRatio > 0}
                  <em>{lossLabel(lane.lossRatio)}</em>
                {/if}
              </div>
            {/if}
          </div>
        </div>
      </div>
    {/each}
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
    justify-content: space-between;
    gap: 12px;
    padding: 14px;
    border-bottom: 1px solid var(--border);
    background: linear-gradient(180deg, var(--surface-2), transparent);
  }
  .card-head h3 {
    margin: 0;
    font-size: 13px;
    font-weight: 820;
    letter-spacing: -0.02em;
  }
  /* Jargon-term affordance on the profile heading (§14.3). */
  .card-head h3.term {
    cursor: help;
    text-decoration: underline dotted color-mix(in srgb, var(--text-soft) 70%, transparent);
    text-underline-offset: 3px;
  }
  .card-head h3.term:focus-visible {
    outline: 2px solid color-mix(in srgb, var(--brand) 70%, transparent);
    outline-offset: 2px;
    border-radius: var(--radius-xs);
  }
  .card-head p {
    margin: 0;
    font-family: var(--font-mono);
    font-size: 10px;
    font-weight: 700;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--text-soft);
  }

  .lanes {
    display: grid;
    gap: 10px;
    padding: 14px;
    min-width: 0;
  }

  .lane {
    display: grid;
    gap: 8px;
    min-width: 0;
    border: 1px solid color-mix(in srgb, var(--border) 72%, transparent);
    border-radius: var(--radius-md);
    background: var(--surface-inset);
    padding: 10px 12px;
  }
  .lane[data-active="true"] {
    border-color: color-mix(in srgb, var(--signal) 44%, var(--border));
    background: color-mix(in srgb, var(--signal-soft) 70%, var(--surface-inset));
  }

  .lane-meta {
    display: flex;
    align-items: baseline;
    gap: 8px;
    min-width: 0;
  }
  .lane-meta span {
    flex: 1;
    overflow: hidden;
    color: var(--text-soft);
    font-family: var(--font-mono);
    font-size: 10px;
    font-weight: 800;
    letter-spacing: 0.06em;
    text-overflow: ellipsis;
    text-transform: uppercase;
    white-space: nowrap;
  }
  .lane-meta strong {
    color: var(--text);
    font-family: var(--font-mono);
    font-size: 13px;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
    line-height: 1;
  }
  .lane-meta em {
    color: var(--text-muted);
    font-family: var(--font-mono);
    font-size: 10px;
    font-style: normal;
    font-variant-numeric: tabular-nums;
  }

  .strip {
    display: grid;
    gap: 6px;
    min-width: 0;
  }
  .ticks {
    position: relative;
    height: 13px;
    margin: 0 4px;
  }
  .ticks span {
    position: absolute;
    top: 0;
    color: var(--text-soft);
    font-family: var(--font-mono);
    font-size: 9px;
    font-variant-numeric: tabular-nums;
    opacity: 0.7;
    transform: translateX(-50%);
    white-space: nowrap;
  }
  .ticks span:first-child {
    transform: none;
  }
  .ticks span:last-child {
    transform: translateX(-100%);
  }

  .track {
    position: relative;
    height: 30px;
    overflow: visible;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background:
      linear-gradient(90deg, var(--border-subtle) 1px, transparent 1px) 0 0 / 25% 100%,
      var(--surface-2);
    cursor: crosshair;
    isolation: isolate;
  }

  /* min–max range bar with end caps */
  .range {
    position: absolute;
    top: 13px;
    height: 5px;
    min-width: 10px;
    border-radius: 999px;
    background: color-mix(in srgb, var(--text-soft) 40%, transparent);
  }
  .range::before,
  .range::after {
    position: absolute;
    top: -7px;
    width: 1px;
    height: 19px;
    content: "";
    border-radius: 999px;
    background: color-mix(in srgb, var(--text-soft) 64%, transparent);
  }
  .range::before {
    left: 0;
  }
  .range::after {
    right: 0;
  }

  /* P10–P90 percentile band */
  .band {
    position: absolute;
    top: 6px;
    height: 18px;
    min-width: 8px;
    border-radius: 999px;
    background: color-mix(in srgb, var(--signal) 28%, transparent);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--signal-strong) 22%, transparent);
  }
  .lane[data-tone="download"] .band {
    background: color-mix(in srgb, var(--phase-download) 28%, transparent);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--phase-download) 30%, transparent);
  }
  .lane[data-tone="upload"] .band {
    background: color-mix(in srgb, var(--phase-upload) 28%, transparent);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--phase-upload) 30%, transparent);
  }

  .avg-marker,
  .cur-marker {
    position: absolute;
    transform: translateX(-50%);
  }
  .avg-marker {
    top: 5px;
    bottom: 5px;
    width: 2px;
    border-radius: 999px;
    background: color-mix(in srgb, var(--text) 54%, transparent);
  }
  .cur-marker {
    top: 9px;
    bottom: 9px;
    width: 10px;
    border: 2px solid var(--surface-1);
    border-radius: 999px;
    background: var(--signal-strong);
    box-shadow: 0 0 0 2px color-mix(in srgb, var(--signal) 18%, transparent);
  }
  .lane[data-tone="download"] .cur-marker {
    background: var(--phase-download);
    box-shadow: 0 0 0 2px color-mix(in srgb, var(--phase-download) 22%, transparent);
  }
  .lane[data-tone="upload"] .cur-marker {
    background: var(--phase-upload);
    box-shadow: 0 0 0 2px color-mix(in srgb, var(--phase-upload) 22%, transparent);
  }

  .loss-marker {
    position: absolute;
    top: 0;
    right: 0;
    bottom: 0;
    min-width: 8px;
    border-radius: 999px;
    background: repeating-linear-gradient(
      -45deg,
      var(--err) 0 4px,
      color-mix(in srgb, var(--err) 44%, transparent) 4px 8px
    );
    opacity: 0.82;
  }

  /* hover affordances */
  .guide {
    position: absolute;
    z-index: 4;
    top: -4px;
    bottom: -4px;
    width: 1px;
    border-radius: 999px;
    background: color-mix(in srgb, var(--text) 54%, transparent);
    pointer-events: none;
    transform: translateX(-50%);
  }
  .pin {
    position: absolute;
    z-index: 5;
    top: 50%;
    width: 11px;
    height: 11px;
    border: 2px solid var(--surface-1);
    border-radius: 999px;
    background: var(--signal-strong);
    box-shadow: 0 0 0 2px color-mix(in srgb, var(--signal) 18%, transparent);
    pointer-events: none;
    transform: translate(-50%, -50%);
  }
  .pin.avg {
    width: 8px;
    border-radius: 2px;
    background: var(--text);
  }
  .lane[data-tone="download"] .pin {
    background: var(--phase-download);
    box-shadow: 0 0 0 2px color-mix(in srgb, var(--phase-download) 22%, transparent);
  }
  .lane[data-tone="upload"] .pin {
    background: var(--phase-upload);
    box-shadow: 0 0 0 2px color-mix(in srgb, var(--phase-upload) 22%, transparent);
  }

  .hover-card {
    position: absolute;
    z-index: 10;
    top: 50%;
    display: grid;
    gap: 4px;
    min-width: 156px;
    max-width: min(238px, 76vw);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-sm);
    background: var(--surface-2);
    padding: 8px 9px;
    box-shadow: var(--shadow-float);
    pointer-events: none;
    transform: translate(12px, -50%);
  }
  .hover-card.left {
    transform: translate(calc(-100% - 12px), -50%);
  }
  .hc-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    min-width: 0;
  }
  .hc-head span,
  .hover-card p,
  .hover-card > em {
    overflow: hidden;
    margin: 0;
    color: var(--text-soft);
    font-family: var(--font-mono);
    font-size: 10px;
    font-style: normal;
    font-weight: 800;
    letter-spacing: 0.04em;
    text-overflow: ellipsis;
    text-transform: uppercase;
    white-space: nowrap;
  }
  .hc-head strong {
    color: var(--text);
    font-family: var(--font-mono);
    font-size: 12px;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .hover-card p,
  .hover-card > em {
    letter-spacing: 0;
    text-transform: none;
  }
  .hover-card > em {
    color: var(--err);
  }

  @media (max-width: 680px) {
    .card-head p {
      display: none;
    }
    .lane-meta {
      flex-wrap: wrap;
    }
  }

  /* ===== Compact variant — embedded under the gauge (§14.2) =====
     Reclaims vertical space so the default main view never scrolls: header
     dropped (~46px) for a tiny inline caption, tighter padding/gaps, no ticks
     row, a shorter track, and the verbose min–max range text dropped so each
     lane is one tight row. Default/full styles above are untouched. */
  .compact {
    /* Sits inside the gauge card already — drop its own chrome so it reads as
       one continuous instrument rather than a nested card. */
    border: 0;
    border-top: 1px solid var(--border);
    border-radius: 0;
    background: transparent;
    box-shadow: none;
  }
  .compact .card-head {
    display: none;
  }
  .compact .compact-caption {
    display: block;
    padding: 8px 2px 0;
    color: var(--text-soft);
    font-family: var(--font-mono);
    font-size: 10px;
    font-weight: 800;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    cursor: help;
  }
  .compact .lanes {
    gap: 6px;
    padding: 8px 2px 2px;
  }
  .compact .lane {
    gap: 4px;
    padding: 6px 8px;
  }
  .compact .lane-meta em {
    /* Drop the verbose min–max range so each lane stays a single tight row. */
    display: none;
  }
  .compact .strip {
    gap: 0;
  }
  .compact .ticks {
    /* The shared axis ticks aren't needed in the dense strip. */
    display: none;
  }
  .compact .track {
    height: 22px;
  }
  .compact .range {
    top: 9px;
  }
  .compact .range::before,
  .compact .range::after {
    top: -6px;
    height: 16px;
  }
  .compact .band {
    top: 4px;
    height: 14px;
  }
</style>
