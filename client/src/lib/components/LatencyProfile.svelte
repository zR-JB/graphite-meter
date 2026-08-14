<script lang="ts">
  // Latency profile view: renders idle and loaded latency lanes with distribution
  // bands, current values, jitter, and loss.
  import { store } from "../state/store.svelte";
  import type { TransportRole } from "../runner/contract";
  import { fmtMs, niceDomain } from "../format";
  import { tooltip, JARGON } from "../actions/tooltip";
  import {
    type MetricKey,
    metricLabel,
    pos as domainPos,
    rangeWidth as domainRangeWidth,
    tickLabel,
    lossLabel,
    metricValue,
    nearestMetric,
    hoverContext,
  } from "./latencyProfile";

  interface Props {
    bare?: boolean;
  }
  let { bare = false }: Props = $props();

  const PROFILE_HELP =
    "Latency profile: phase-aligned bucket medians show typical responsiveness; the outer range preserves measured spikes, and loss uses every ping outcome. Tighter is steadier.";

  const LANE_META: Record<TransportRole, { label: string; tone: string }> = {
    latency: { label: "Idle", tone: "idle" },
    download: { label: "Loaded Down", tone: "download" },
    upload: { label: "Loaded Up", tone: "upload" },
    bidirectional: { label: "Loaded Bi-dir", tone: "bidirectional" },
  };

  const lanes = $derived(
    store.latencyLanes.filter(
      (lane) => store.stagePresentation[lane.key].configured,
    ),
  );

  const domain = $derived.by(() => {
    const values: number[] = [];
    for (const lane of lanes) {
      if (lane.min != null) values.push(lane.min);
      if (lane.max != null) values.push(lane.max);
    }
    return niceDomain(values, { floor: 1 });
  });

  const ticks = $derived([
    domain.min,
    domain.min + domain.span / 2,
    domain.max,
  ]);

  let hover = $state<{
    key: TransportRole;
    metric: MetricKey;
    anchorPct: number;
    trackWidth: number;
  } | null>(null);
  let cardWidth = $state(0);

  const hoverLane = $derived(
    hover ? (lanes.find((l) => l.key === hover!.key) ?? null) : null,
  );
  const hoverValue = $derived(
    hoverLane && hover ? metricValue(hoverLane, hover.metric) : null,
  );

  const CARD_GAP = 12;
  const CARD_PAD = 6;
  const cardLeft = $derived.by(() => {
    if (!hover) return 0;
    const anchorPx = (hover.anchorPct / 100) * hover.trackWidth;
    const preferRight = hover.anchorPct <= 50;
    const desired = preferRight
      ? anchorPx + CARD_GAP
      : anchorPx - cardWidth - CARD_GAP;
    const maxLeft = Math.max(CARD_PAD, hover.trackWidth - cardWidth - CARD_PAD);
    return Math.min(Math.max(CARD_PAD, desired), maxLeft);
  });

  // ./latencyProfile holds the domain-free helpers. These wrappers bind the
  // reactive chart domain, keeping the template call sites terse.
  const pos = (value: number | null) => domainPos(value, domain);
  const rangeWidth = (min: number | null, max: number | null) =>
    domainRangeWidth(min, max, domain);

  function onStripMove(e: PointerEvent, key: TransportRole) {
    const lane = lanes.find((l) => l.key === key);
    if (!lane) return;
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const ratio = Math.min(
      1,
      Math.max(0, (e.clientX - rect.left) / rect.width),
    );
    const metric = nearestMetric(lane, domain.min + ratio * domain.span);
    if (!metric) {
      hover = null;
      return;
    }
    const value = metricValue(lane, metric);
    if (value == null) {
      hover = null;
      return;
    }
    hover = { key, metric, anchorPct: pos(value), trackWidth: rect.width };
  }
  function clearHover() {
    hover = null;
  }
</script>

<section class="card" class:bare aria-label="Latency distribution">
  <header class="card-head">
    <h3 class="term" use:tooltip={PROFILE_HELP}>Latency Profile</h3>
    <p>Median range / spikes / loss</p>
  </header>

  {#if store.stagePresentation.latency.status === "failed"}
    <p class="lane-fail" role="alert">
      Latency skipped — {store.stagePresentation.latency.failure
        ? store.stageFailures.latency?.message
        : "unavailable"}
    </p>
  {/if}

  <div class="lanes" role="img" aria-label="Latency, jitter and loss by phase">
    {#each lanes as lane (lane.key)}
      {@const meta = LANE_META[lane.key]}
      <div
        class="lane"
        data-tone={meta.tone}
        data-active={lane.active}
        data-enabled={store.stagePresentation[lane.key].configured}
      >
        <div class="lane-meta">
          <span>{meta.label}</span>
          <strong
            >{lane.center == null
              ? "waiting"
              : lane.centerKind === "result"
                ? `${fmtMs(lane.center)} ms`
                : `avg ${fmtMs(lane.center)}`}</strong
          >
          {#if lane.jitter != null}
            <em class="jit" use:tooltip={JARGON.jitter}
              >± {fmtMs(lane.jitter)} jit</em
            >
          {/if}
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
              <span
                class="range"
                style="left:{pos(lane.min)}%;width:{rangeWidth(
                  lane.min,
                  lane.max,
                )}%"
              ></span>
            {/if}
            {#if lane.p10 != null && lane.p90 != null}
              <span
                class="band"
                style="left:{pos(lane.p10)}%;width:{rangeWidth(
                  lane.p10,
                  lane.p90,
                )}%"
              ></span>
            {/if}
            {#if lane.center != null}
              <i class="center-marker" style="left:{pos(lane.center)}%"></i>
            {/if}
            {#if lane.current != null}
              <i class="cur-marker" style="left:{pos(lane.current)}%"></i>
            {/if}
            {#if lane.lossRatio > 0}
              <i
                class="loss-marker"
                use:tooltip={{ text: lossLabel(lane.lossRatio), instant: true }}
                style="width:{Math.min(34, Math.max(8, lane.lossRatio * 100))}%"
              ></i>
            {/if}

            {#if hover?.key === lane.key && hoverValue != null}
              <span class="guide" style="left:{pos(hoverValue)}%"></span>
              <span
                class="pin"
                class:center={hover.metric === "center"}
                style="left:{pos(hoverValue)}%"
              ></span>
              <div
                class="hover-card"
                bind:clientWidth={cardWidth}
                style="left:{cardLeft}px"
              >
                <div class="hc-head">
                  <span>{meta.label}</span>
                  <strong
                    >{metricLabel(lane, hover.metric)}
                    {fmtMs(hoverValue)}</strong
                  >
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
    border-radius: var(--r-chrome);
    background: var(--surface-1);
    box-shadow: var(--elev-tile);
    overflow: clip;
  }
  .card-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
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
  .card-head h3.term {
    cursor: help;
    text-decoration: underline dotted
      color-mix(in srgb, var(--text-soft) 70%, transparent);
    text-underline-offset: 3px;
  }
  .card-head h3.term:focus-visible {
    outline: var(--focus-ring);
    outline-offset: 2px;
    border-radius: var(--r-well);
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

  .lane-fail {
    margin: 14px 14px 0;
    padding: var(--space-1) var(--space-2);
    border: 1px solid color-mix(in srgb, var(--err) 40%, var(--border));
    border-radius: var(--r-chrome);
    background: var(--err-soft);
    color: var(--err);
    font-size: 11.5px;
    font-weight: 600;
    line-height: 1.35;
  }
  .bare .lane-fail {
    margin: 0 0 var(--space-2);
  }

  .lane {
    display: grid;
    gap: var(--space-2);
    min-width: 0;
    border: 1px solid color-mix(in srgb, var(--border) 72%, transparent);
    border-radius: var(--r-well);
    background: var(--surface-inset);
    padding: var(--space-2) var(--space-3);
  }
  .lane[data-active="true"] {
    border-color: color-mix(in srgb, var(--signal) 44%, var(--border));
    background: color-mix(
      in srgb,
      var(--signal-soft) 70%,
      var(--surface-inset)
    );
  }
  .lane[data-enabled="false"] {
    opacity: 0.5;
  }

  .lane-meta {
    display: flex;
    align-items: baseline;
    gap: var(--space-2);
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
  .lane-meta .jit {
    cursor: help;
    text-decoration: underline dotted
      color-mix(in srgb, var(--text-soft) 70%, transparent);
    text-underline-offset: 2px;
  }

  .strip {
    display: grid;
    gap: 6px;
    min-width: 0;
  }
  .ticks {
    position: relative;
    height: 13px;
    margin: 0 var(--space-1);
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
    border-radius: var(--r-well);
    background:
      linear-gradient(90deg, var(--border-subtle) 1px, transparent 1px) 0 0 /
        25% 100%,
      var(--surface-2);
    cursor: crosshair;
    isolation: isolate;
  }

  .range {
    position: absolute;
    top: 13px;
    height: 5px;
    min-width: 10px;
    border-radius: var(--r-full);
    background: color-mix(in srgb, var(--text-soft) 40%, transparent);
  }
  .range::before,
  .range::after {
    position: absolute;
    top: -7px;
    width: 1px;
    height: 19px;
    content: "";
    border-radius: var(--r-full);
    background: color-mix(in srgb, var(--text-soft) 64%, transparent);
  }
  .range::before {
    left: 0;
  }
  .range::after {
    right: 0;
  }

  .band {
    position: absolute;
    top: 6px;
    height: 18px;
    min-width: 8px;
    border-radius: var(--r-full);
    background: color-mix(in srgb, var(--signal) 28%, transparent);
    box-shadow: inset 0 0 0 1px
      color-mix(in srgb, var(--signal-strong) 22%, transparent);
  }
  .lane[data-tone="download"] .band {
    background: color-mix(in srgb, var(--phase-download) 28%, transparent);
    box-shadow: inset 0 0 0 1px
      color-mix(in srgb, var(--phase-download) 30%, transparent);
  }
  .lane[data-tone="upload"] .band {
    background: color-mix(in srgb, var(--phase-upload) 28%, transparent);
    box-shadow: inset 0 0 0 1px
      color-mix(in srgb, var(--phase-upload) 30%, transparent);
  }

  .center-marker,
  .cur-marker {
    position: absolute;
    transform: translateX(-50%);
  }
  .center-marker {
    top: 5px;
    bottom: 5px;
    width: 2px;
    border-radius: var(--r-full);
    background: color-mix(in srgb, var(--text) 54%, transparent);
  }
  .cur-marker {
    top: 9px;
    bottom: 9px;
    width: 10px;
    border: 2px solid var(--surface-1);
    border-radius: var(--r-full);
    background: var(--signal-strong);
    box-shadow: 0 0 0 2px color-mix(in srgb, var(--signal) 18%, transparent);
  }
  .lane[data-tone="download"] .cur-marker {
    background: var(--phase-download);
    box-shadow: 0 0 0 2px
      color-mix(in srgb, var(--phase-download) 22%, transparent);
  }
  .lane[data-tone="upload"] .cur-marker {
    background: var(--phase-upload);
    box-shadow: 0 0 0 2px
      color-mix(in srgb, var(--phase-upload) 22%, transparent);
  }

  .loss-marker {
    position: absolute;
    top: 0;
    right: 0;
    bottom: 0;
    min-width: 8px;
    border-radius: var(--r-full);
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
    border-radius: var(--r-full);
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
    border-radius: var(--r-full);
    background: var(--signal-strong);
    box-shadow: 0 0 0 2px color-mix(in srgb, var(--signal) 18%, transparent);
    pointer-events: none;
    transform: translate(-50%, -50%);
  }
  .pin.center {
    width: 8px;
    border-radius: var(--r-well);
    background: var(--text);
  }
  .lane[data-tone="download"] .pin {
    background: var(--phase-download);
    box-shadow: 0 0 0 2px
      color-mix(in srgb, var(--phase-download) 22%, transparent);
  }
  .lane[data-tone="upload"] .pin {
    background: var(--phase-upload);
    box-shadow: 0 0 0 2px
      color-mix(in srgb, var(--phase-upload) 22%, transparent);
  }

  .hover-card {
    position: absolute;
    z-index: 10;
    top: 50%;
    display: grid;
    gap: var(--space-1);
    min-width: 156px;
    max-width: min(238px, 76vw);
    border: 1px solid var(--border-strong);
    border-radius: var(--r-chrome);
    background: var(--surface-2);
    padding: 8px 9px;
    box-shadow: var(--shadow-float);
    pointer-events: none;
    /* `left` is pre-offset + clamped in JS; only the vertical centering here. */
    transform: translateY(-50%);
  }
  .hc-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
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

  @media (max-width: 759px) {
    /* bp: stacked */
    .card-head p {
      display: none;
    }
    .lane-meta {
      flex-wrap: wrap;
    }
  }

  /* Bare variant: a peer of the gauge inside a host panel. The host's inset
     surface provides the chrome, so the card frame and header drop out. Lanes
     take a raised backdrop, each reading as its own row. */
  .bare {
    border: 0;
    border-radius: 0;
    background: transparent;
    box-shadow: none;
    overflow: visible;
    height: 100%;
  }
  .bare .card-head {
    display: none;
  }
  .bare .lanes {
    gap: var(--space-2);
    padding: 0;
  }
  .bare .lane {
    /* Flat milled row resting over the inset host (the gauge-row well). */
    background: var(--surface-1);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-well);
    box-shadow: var(--elev-tile);
    padding: var(--space-2) var(--space-3);
  }
  .bare .lane[data-active="true"] {
    border-color: color-mix(in srgb, var(--signal) 44%, var(--border));
    background: color-mix(in srgb, var(--signal-soft) 60%, var(--surface-1));
  }
</style>
