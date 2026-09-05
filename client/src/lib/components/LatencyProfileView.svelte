<script lang="ts">
  import { tooltip, JARGON } from "../actions/tooltip";
  import { ICON } from "../constants";
  import { fmtMs } from "../format";
  import {
    entries,
    probeAccountingHelp,
    probeAccountingDetails,
    hasProbeAccountingNotice,
    hoverContext,
    timeoutLabel,
    metricLabel,
    metricValue,
    nearestMetric,
    pos,
    profileDomain,
    rangeWidth,
    tickLabel,
    type LatencyProfileDomain,
    type LatencyProfileViewLane,
    type MetricKey,
  } from "./latencyProfile";

  interface Props {
    lanes: LatencyProfileViewLane[];
    domain?: LatencyProfileDomain;
    variant?: "bare" | "compact";
    showCurrent?: boolean;
    showTimeouts?: boolean;
    jitterDescription?: string;
    label?: string;
  }

  let {
    lanes,
    domain,
    variant = "bare",
    showCurrent = false,
    showTimeouts = false,
    jitterDescription = JARGON.jitter,
    label = "Latency, jitter and probe timeouts by phase",
  }: Props = $props();

  const scale = $derived(domain ?? profileDomain(lanes));
  const ticks = $derived([
    scale.min,
    scale.min + scale.span / 2,
    scale.min + scale.span,
  ]);
  const laneIcons = {
    latency: ICON.ping,
    download: ICON.download,
    upload: ICON.upload,
    bidirectional: ICON.bidirectional,
  } as const;

  let hover = $state<{
    key: LatencyProfileViewLane["key"];
    metric: MetricKey;
    anchorPct: number;
    trackWidth: number;
  } | null>(null);
  let keyboardLane = $state<LatencyProfileViewLane["key"] | null>(null);
  let cardWidth = $state(0);

  const hoverLane = $derived(
    hover ? (lanes.find((lane) => lane.key === hover!.key) ?? null) : null,
  );
  const hoverValue = $derived(
    hoverLane && hover ? metricValue(hoverLane, hover.metric) : null,
  );

  const CARD_GAP = 12;
  const CARD_PAD = 6;
  const cardLeft = $derived.by(() => {
    if (!hover) return 0;
    const anchorPx = (hover.anchorPct / 100) * hover.trackWidth;
    const desired =
      hover.anchorPct <= 50
        ? anchorPx + CARD_GAP
        : anchorPx - cardWidth - CARD_GAP;
    const maxLeft = Math.max(CARD_PAD, hover.trackWidth - cardWidth - CARD_PAD);
    return Math.min(Math.max(CARD_PAD, desired), maxLeft);
  });

  function setHover(
    lane: LatencyProfileViewLane,
    metric: MetricKey,
    track: HTMLElement,
  ) {
    const value = metricValue(lane, metric);
    if (value == null) return;
    hover = {
      key: lane.key,
      metric,
      anchorPct: pos(value, scale),
      trackWidth: track.getBoundingClientRect().width,
    };
  }

  function onTrackMove(event: PointerEvent, lane: LatencyProfileViewLane) {
    const track = event.currentTarget as HTMLElement;
    const rect = track.getBoundingClientRect();
    const ratio = Math.min(
      1,
      Math.max(0, (event.clientX - rect.left) / rect.width),
    );
    const metric = nearestMetric(lane, scale.min + ratio * scale.span);
    if (metric) setHover(lane, metric, track);
  }

  function onTrackFocus(event: FocusEvent, lane: LatencyProfileViewLane) {
    keyboardLane = lane.key;
    const metrics = entries(lane);
    const preferred = metrics.find((entry) => entry.metric === "center");
    const metric = preferred?.metric ?? metrics[0]?.metric;
    if (metric) setHover(lane, metric, event.currentTarget as HTMLElement);
  }

  function onTrackKey(event: KeyboardEvent, lane: LatencyProfileViewLane) {
    const metrics = entries(lane);
    if (!metrics.length) return;
    if (event.key === "Escape") {
      hover = null;
      return;
    }
    if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
    event.preventDefault();
    const current = metrics.findIndex(
      (entry) => hover?.key === lane.key && entry.metric === hover.metric,
    );
    const next =
      event.key === "Home"
        ? 0
        : event.key === "End"
          ? metrics.length - 1
          : event.key === "ArrowRight"
            ? Math.min(metrics.length - 1, Math.max(0, current + 1))
            : Math.max(0, current < 0 ? metrics.length - 1 : current - 1);
    setHover(lane, metrics[next].metric, event.currentTarget as HTMLElement);
  }

  function accessibleLane(lane: LatencyProfileViewLane): string {
    const values = [
      lane.center == null
        ? null
        : `${metricLabel(lane, "center")} ${fmtMs(lane.center)} milliseconds`,
      lane.min == null || lane.max == null
        ? null
        : `range ${fmtMs(lane.min)} to ${fmtMs(lane.max)} milliseconds`,
      lane.p10 == null || lane.p90 == null
        ? null
        : `P10 to P90 ${fmtMs(lane.p10)} to ${fmtMs(lane.p90)} milliseconds`,
      lane.jitter == null ? null : `jitter ${fmtMs(lane.jitter)} milliseconds`,
      showTimeouts && lane.timeoutRatio != null && lane.timeoutRatio > 0
        ? timeoutLabel(lane.timeoutRatio)
        : null,
      lane.accountingComplete === false ? "Partial accounting" : null,
      probeAccountingDetails(lane) || null,
    ].filter((value): value is string => value !== null);
    return `${lane.label} latency profile${values.length ? `. ${values.join(". ")}` : ". Waiting for measurements"}`;
  }
</script>

<div
  class="lanes"
  data-latency-profile
  data-variant={variant}
  role="group"
  aria-label={label}
>
  <div class="ticks" aria-hidden="true">
    {#each ticks as tick, index (index)}
      <span style={`left:${pos(tick, scale)}%`}>{tickLabel(tick)} ms</span>
    {/each}
  </div>
  {#each lanes as lane (lane.key)}
    <div class="lane" data-tone={lane.tone} data-active={lane.active === true}>
      <div class="lane-meta">
        <span class="lane-icon" aria-hidden="true"
          >{@html laneIcons[lane.key]}</span
        >
        <span class="lane-label">{lane.label}</span>
        <strong
          >{lane.center == null
            ? lane.accountingComplete === false || (lane.count ?? 0) > 0
              ? "unavailable"
              : "waiting"
            : lane.centerKind === "average"
              ? `avg ${fmtMs(lane.center)} ms`
              : `${fmtMs(lane.center)} ms`}</strong
        >
        {#if lane.jitter != null}
          <em class="jit" use:tooltip={jitterDescription}
            >{fmtMs(lane.jitter)} ms jitter</em
          >
        {/if}
        <em class="range-label">
          {lane.min == null || lane.max == null
            ? "range —"
            : `${fmtMs(lane.min)} – ${fmtMs(lane.max)}`}
        </em>
      </div>

      {#if hasProbeAccountingNotice(lane)}
        <p class="probe-accounting">
          {#if lane.accountingComplete === false}
            <span
              class="accounting-warning"
              role="note"
              use:tooltip={probeAccountingHelp(lane)}>Partial accounting</span
            >
          {/if}
          <span>{probeAccountingDetails(lane)}</span>
        </p>
      {/if}
      <div class="strip">
        <button
          type="button"
          class="track"
          aria-label={accessibleLane(lane)}
          disabled={entries(lane).length === 0}
          onpointermove={(event) => onTrackMove(event, lane)}
          onpointerleave={() => {
            if (keyboardLane !== lane.key) hover = null;
          }}
          onfocus={(event) => onTrackFocus(event, lane)}
          onblur={() => {
            keyboardLane = null;
            hover = null;
          }}
          onkeydown={(event) => onTrackKey(event, lane)}
        >
          {#if lane.min != null && lane.max != null}
            <span
              class="range"
              style={`left:${pos(lane.min, scale)}%;width:${rangeWidth(lane.min, lane.max, scale)}%`}
            ></span>
          {/if}
          {#if lane.p10 != null && lane.p90 != null}
            <span
              class="band"
              style={`left:${pos(lane.p10, scale)}%;width:${rangeWidth(lane.p10, lane.p90, scale)}%`}
            ></span>
          {/if}
          {#if lane.center != null}
            <i class="center-marker" style={`left:${pos(lane.center, scale)}%`}
            ></i>
          {/if}
          {#if showCurrent && lane.current != null}
            <i
              class="current-marker"
              style={`left:${pos(lane.current, scale)}%`}
            ></i>
          {/if}
          {#if showTimeouts && lane.timeoutRatio != null && lane.timeoutRatio > 0}
            <i
              class="timeout-marker"
              style={`width:${Math.min(34, Math.max(8, lane.timeoutRatio * 100))}%`}
            ></i>
          {/if}

          {#if hover?.key === lane.key && hoverValue != null}
            <span class="guide" style={`left:${pos(hoverValue, scale)}%`}
            ></span>
            <span
              class="pin"
              class:center={hover.metric === "center"}
              style={`left:${pos(hoverValue, scale)}%`}
            ></span>
            <span
              class="hover-card"
              bind:clientWidth={cardWidth}
              style={`left:${cardLeft}px`}
            >
              <span class="hover-head">
                <span>{lane.label}</span>
                <strong
                  >{metricLabel(lane, hover.metric)}
                  {fmtMs(hoverValue)}</strong
                >
              </span>
              {#if hoverContext(lane, hover.metric)}
                <span class="hover-context"
                  >{hoverContext(lane, hover.metric)}</span
                >
              {/if}
              {#if showTimeouts && lane.timeoutRatio != null && lane.timeoutRatio > 0}
                <em>{timeoutLabel(lane.timeoutRatio)}</em>
              {/if}
            </span>
          {/if}
        </button>
      </div>
    </div>
  {/each}
</div>

<style>
  .probe-accounting {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-1) var(--space-2);
    margin: 0;
    color: var(--text-muted);
    font-size: 11px;
  }
  .accounting-warning {
    color: var(--warn);
    font-weight: 600;
  }
  .lanes {
    display: grid;
    gap: 6px;
    min-width: 0;
    padding: 0;
  }
  .lane {
    --tone: var(--phase-latency);
    display: grid;
    gap: var(--space-1);
    min-width: 0;
    padding: 6px var(--space-3);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-well);
    background: var(--surface-1);
    box-shadow: var(--elev-tile);
  }
  .lane[data-tone="download"] {
    --tone: var(--phase-download);
  }
  .lane[data-tone="upload"] {
    --tone: var(--phase-upload);
  }
  .lane[data-tone="bidirectional"] {
    --tone: var(--phase-bidirectional);
  }
  .lane[data-active="true"] {
    border-color: color-mix(in srgb, var(--tone) 44%, var(--border));
    background: color-mix(
      in srgb,
      var(--signal-soft) 70%,
      var(--surface-inset)
    );
  }
  .lane-meta {
    display: flex;
    align-items: baseline;
    flex-wrap: wrap;
    gap: var(--space-1) var(--space-2);
    min-width: 0;
  }
  .lane-icon {
    display: grid;
    place-items: center;
    width: 18px;
    height: 18px;
    flex: none;
    align-self: center;
    border: 1px solid color-mix(in srgb, var(--tone) 32%, var(--border));
    border-radius: var(--r-well);
    background: var(--surface-2);
    color: var(--tone);
  }
  .lane-icon :global(svg) {
    width: 11px;
    height: 11px;
  }
  .lane-label {
    min-width: 0;
    flex: 1;
    overflow: hidden;
    color: var(--text-muted);
    font: 800 10px var(--font-mono);
    letter-spacing: 0.06em;
    text-overflow: ellipsis;
    text-transform: uppercase;
    white-space: nowrap;
  }
  .lane-meta strong {
    color: var(--text);
    font: 700 13px var(--font-mono);
    font-variant-numeric: tabular-nums;
    line-height: 1;
  }
  .lane-meta em {
    color: var(--text-muted);
    font: 400 10px var(--font-mono);
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
    margin: 0 calc(var(--space-3) + 1px);
  }
  .ticks span {
    position: absolute;
    top: 0;
    color: var(--text-muted);
    font: 400 9px var(--font-mono);
    font-variant-numeric: tabular-nums;
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
    width: 100%;
    padding: 0;
    overflow: visible;
    border: 1px solid var(--border);
    border-radius: var(--r-well);
    background:
      linear-gradient(90deg, var(--border-subtle) 1px, transparent 1px) 0 0 /
        25% 100%,
      var(--surface-2);
    cursor: crosshair;
    color: inherit;
    font: inherit;
    isolation: isolate;
  }
  .track:disabled {
    cursor: default;
  }
  .track:focus-visible {
    outline: var(--focus-ring);
    outline-offset: 2px;
  }
  .range,
  .band,
  .center-marker,
  .current-marker,
  .timeout-marker {
    position: absolute;
  }
  .range {
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
    background: color-mix(in srgb, var(--text-soft) 64%, transparent);
  }
  .range::before {
    left: 0;
  }
  .range::after {
    right: 0;
  }
  .band {
    top: 6px;
    height: 18px;
    min-width: 8px;
    border-radius: var(--r-full);
    background: color-mix(in srgb, var(--tone) 28%, transparent);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--tone) 30%, transparent);
  }
  .center-marker,
  .current-marker {
    transform: translateX(-50%);
  }
  .center-marker {
    top: 5px;
    bottom: 5px;
    width: 2px;
    border-radius: var(--r-full);
    background: color-mix(in srgb, var(--text) 54%, transparent);
  }
  .current-marker {
    top: 9px;
    bottom: 9px;
    width: 10px;
    border: 2px solid var(--surface-1);
    border-radius: var(--r-full);
    background: var(--tone);
    box-shadow: 0 0 0 2px color-mix(in srgb, var(--tone) 20%, transparent);
  }
  .timeout-marker {
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
    background: var(--tone);
    box-shadow: 0 0 0 2px color-mix(in srgb, var(--tone) 22%, transparent);
    pointer-events: none;
    transform: translate(-50%, -50%);
  }
  .pin.center {
    width: 8px;
    border-radius: var(--r-well);
    background: var(--text);
  }
  .hover-card {
    position: absolute;
    z-index: 10;
    top: 50%;
    display: grid;
    gap: var(--space-1);
    min-width: 156px;
    max-width: min(238px, 76vw);
    padding: 8px 9px;
    border: 1px solid var(--border-strong);
    border-radius: var(--r-chrome);
    background: var(--surface-2);
    box-shadow: var(--shadow-float);
    pointer-events: none;
    transform: translateY(-50%);
  }
  .hover-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
    min-width: 0;
  }
  .hover-head span,
  .hover-context,
  .hover-card > em {
    overflow: hidden;
    margin: 0;
    color: var(--text-muted);
    font: 800 10px var(--font-mono);
    font-style: normal;
    letter-spacing: 0.04em;
    text-overflow: ellipsis;
    text-transform: uppercase;
    white-space: nowrap;
  }
  .hover-head strong {
    color: var(--text);
    font: 700 12px var(--font-mono);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .hover-context,
  .hover-card > em {
    display: block;
    letter-spacing: 0;
    text-transform: none;
  }
  .hover-card > em {
    color: var(--err);
  }
  .lanes[data-variant="compact"] .lane {
    padding: 9px 10px 10px;
  }
  .lanes[data-variant="compact"] .lane-meta strong {
    font-size: var(--type-sm);
  }
  .lanes[data-variant="compact"] .track {
    height: 24px;
  }
  .lanes[data-variant="compact"] .range {
    top: 10px;
  }
  .lanes[data-variant="compact"] .range::before,
  .lanes[data-variant="compact"] .range::after {
    top: -5px;
    height: 15px;
  }
  .lanes[data-variant="compact"] .band {
    top: 4px;
    height: 14px;
  }
  .lanes[data-variant="compact"] .center-marker {
    top: 3px;
    bottom: 3px;
  }
  .lanes[data-variant="compact"] .current-marker {
    top: 6px;
    bottom: 6px;
  }
  @media (max-width: 759px) {
    .lane-meta {
      flex-wrap: wrap;
    }
    .range-label {
      display: none;
    }
  }
</style>
