<script lang="ts">
  import { inView } from "../actions/inView";
  import Icon from "./Icon.svelte";
  import { tooltip } from "../actions/tooltip";
  import { JARGON, MISSING, STAGE } from "../presentation/vocabulary";
  import { fmtMs, fmtMsTick } from "../format";
  import {
    entries,
    PARTIAL_ACCOUNTING_HELP,
    probeAccountingDetails,
    hasProbeAccountingNotice,
    hoverContext,
    reflectorTimingDescription,
    timeoutLabel,
    metricLabel,
    metricValue,
    nearestMetric,
    pos,
    profileDomain,
    rangeWidth,
    type LatencyProfileViewLane,
    type MetricKey,
  } from "./latencyProfile";

  interface Props {
    lanes: LatencyProfileViewLane[];
    variant?: "bare" | "compact";
    label?: string;
  }

  let {
    lanes,
    variant = "bare",
    label = "Latency, jitter and probe timeouts by phase",
  }: Props = $props();

  let motion = $state(false);

  const scale = $derived(profileDomain(lanes));
  const live = $derived(variant === "bare");
  const ticks = $derived([
    scale.min,
    scale.min + scale.span / 2,
    scale.min + scale.span,
  ]);

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
        : `${metricLabel("center")} ${fmtMs(lane.center)} milliseconds`,
      lane.min == null || lane.max == null
        ? null
        : `range ${fmtMs(lane.min)} to ${fmtMs(lane.max)} milliseconds`,
      lane.p10 == null || lane.p90 == null
        ? null
        : `P10 to P90 ${fmtMs(lane.p10)} to ${fmtMs(lane.p90)} milliseconds`,
      lane.p95 == null ? null : `P95 ${fmtMs(lane.p95)} milliseconds`,
      lane.jitter == null ? null : `jitter ${fmtMs(lane.jitter)} milliseconds`,
      live && lane.timeoutRatio != null && lane.timeoutRatio > 0
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
  data-motion={motion && live}
  {@attach live && inView((seen) => (motion = seen))}
  data-variant={variant}
  role="group"
  aria-label={label}
>
  {#each lanes as lane (lane.key)}
    {@const accounting = `${lane.accountingComplete === false ? `Partial accounting. ${PARTIAL_ACCOUNTING_HELP} ` : ""}${probeAccountingDetails(lane)}`}
    {@const metrics = entries(lane)}
    {@const selected =
      hover?.key === lane.key
        ? metrics.findIndex((entry) => entry.metric === hover!.metric)
        : -1}
    <div class="lane" data-tone={lane.key} data-active={lane.active === true}>
      <div class="lane-meta">
        <span class="tone-icon lane-icon" aria-hidden="true"
          ><Icon name={STAGE[lane.key].icon} /></span
        >
        <span class="caps lane-label">{lane.label}</span>
        <strong
          >{lane.center == null
            ? lane.accountingComplete === false || lane.count > 0
              ? "unavailable"
              : "waiting"
            : `median ${fmtMs(lane.center)} ms`}</strong
        >
        {#if lane.jitter != null}
          <em class="term jit" use:tooltip={JARGON.jitter}
            >{fmtMs(lane.jitter)} ms jitter</em
          >
        {/if}
        <em class="range-label">
          {lane.min == null || lane.max == null
            ? `range ${MISSING}`
            : `${fmtMs(lane.min)} – ${fmtMs(lane.max)}`}
        </em>
        <span class="accounting-slot">
          {#if lane.reflectorTiming || hasProbeAccountingNotice(lane)}
            <span
              class="timing-info"
              class:accounting-warning={hasProbeAccountingNotice(lane)}
              role="note"
              aria-label={`${lane.label}: measurement details${hasProbeAccountingNotice(lane) ? `. ${accounting}` : ""}`}
              use:tooltip={[
                lane.reflectorTiming
                  ? reflectorTimingDescription(lane.reflectorTiming)
                  : "",
                hasProbeAccountingNotice(lane) ? accounting : "",
              ]
                .filter(Boolean)
                .join("\n\n")}><Icon name="info" /></span
            >
          {/if}
        </span>
      </div>

      <div
        class="track"
        role="slider"
        tabindex={metrics.length ? 0 : -1}
        aria-label={accessibleLane(lane)}
        aria-disabled={!metrics.length}
        aria-valuemin={0}
        aria-valuemax={Math.max(0, metrics.length - 1)}
        aria-valuenow={Math.max(0, selected)}
        aria-valuetext={selected >= 0 && hover && hoverValue != null
          ? `${metricLabel(hover.metric)} ${fmtMs(hoverValue)} milliseconds`
          : undefined}
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
        <span class="profile-artwork" aria-hidden="true">
          {#if lane.min != null && lane.max != null}
            <span
              class="range"
              style:transform={`translateX(${pos(lane.min, scale)}%) scaleX(${rangeWidth(lane.min, lane.max, scale) / 100})`}
            ></span>
            <span
              class="position"
              style:transform={`translateX(${pos(lane.min, scale)}%)`}
              ><i class="range-cap"></i></span
            >
            <span
              class="position"
              style:transform={`translateX(${pos(lane.max, scale)}%)`}
              ><i class="range-cap"></i></span
            >
          {/if}
          {#if lane.p10 != null && lane.p90 != null}
            <span
              class="band"
              style:transform={`translateX(${pos(lane.p10, scale)}%) scaleX(${rangeWidth(lane.p10, lane.p90, scale) / 100})`}
            ></span>
          {/if}
          {#if lane.center != null}
            <span
              class="position"
              style:transform={`translateX(${pos(lane.center, scale)}%)`}
              ><i class="center-marker"></i></span
            >
          {/if}
          {#if live && lane.current != null}
            <span
              class="position"
              style:transform={`translateX(${pos(lane.current, scale)}%)`}
              ><i class="current-marker"></i></span
            >
          {/if}
          {#if live && lane.timeoutRatio != null && lane.timeoutRatio > 0}
            <i
              class="timeout-marker"
              style={`width:${Math.min(34, Math.max(8, lane.timeoutRatio * 100))}%`}
            ></i>
          {/if}
        </span>
        {#if hover?.key === lane.key && hoverValue != null}
          <span class="guide" style={`left:${pos(hoverValue, scale)}%`}></span>
          <span
            class="inspect-card hover-card"
            bind:clientWidth={cardWidth}
            style={`left:${cardLeft}px`}
          >
            <span class="hover-head">
              <span>{lane.label}</span>
              <strong
                >{metricLabel(hover.metric)}
                {fmtMs(hoverValue)}</strong
              >
            </span>
            {#if hoverContext(lane, hover.metric)}
              <span class="hover-context"
                >{hoverContext(lane, hover.metric)}</span
              >
            {/if}
            {#if live && lane.timeoutRatio != null && lane.timeoutRatio > 0}
              <em>{timeoutLabel(lane.timeoutRatio)}</em>
            {/if}
          </span>
        {/if}
      </div>
    </div>
  {/each}
  <div class="ticks" aria-hidden="true">
    {#each ticks as tick, index (index)}
      <span style={`left:${pos(tick, scale)}%`}
        >{fmtMsTick(tick)}{index === 2 ? " ms" : ""}</span
      >
    {/each}
  </div>
</div>

<style>
  .lanes {
    --lane-pad: var(--space-3);
    display: grid;
    gap: var(--profile-lane-gap, 6px);
    min-width: 0;
    container: latency-lanes / inline-size;
  }
  .lane {
    display: grid;
    gap: 6px;
    min-width: 0;
    padding: 6px var(--lane-pad);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-well);
    background: var(--surface-1);
    box-shadow: var(--elev-tile);
  }
  @media (max-height: 800px) {
    .lanes[data-variant="bare"] {
      gap: var(--space-1);
    }
    .lanes[data-variant="bare"] .lane {
      padding-block: var(--space-1);
    }
  }
  .lane[data-active="true"] {
    border-color: var(--tone-line);
    background: color-mix(
      in srgb,
      var(--signal-soft) 70%,
      var(--surface-inset)
    );
  }
  .lane-meta {
    display: flex;
    align-items: baseline;
    gap: var(--space-1) var(--space-2);
    min-width: 0;
  }
  .lane-icon {
    align-self: center;
    width: 18px;
    height: 18px;
  }
  .lane-icon :global(svg) {
    width: 11px;
    height: 11px;
  }
  .lane-label {
    flex: 1 0 auto;
    color: var(--text-muted);
    white-space: nowrap;
  }
  .lane-meta strong {
    flex: none;
    min-width: 15ch;
    font: var(--w-heavy) var(--type-sm) / 1 var(--font-mono);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .lane-meta em {
    min-width: 0;
    overflow: hidden;
    color: var(--text-muted);
    font: 400 var(--type-2xs) var(--font-mono);
    font-variant-numeric: tabular-nums;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .accounting-slot {
    display: inline-flex;
    flex: 0 0 20px;
    align-self: center;
  }
  .timing-info {
    display: inline-grid;
    place-items: center;
    width: 20px;
    height: 20px;
    color: var(--text-muted);
    cursor: help;
  }
  .timing-info :global(svg) {
    width: 12px;
    height: 12px;
  }
  .accounting-warning {
    color: var(--warn);
  }
  .ticks {
    position: relative;
    height: 13px;
    margin-inline: calc(var(--lane-pad) + 2px);
  }
  .ticks span {
    position: absolute;
    translate: -50%;
    color: var(--text-muted);
    font: var(--type-2xs) / 1.2 var(--font-mono);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .ticks span:first-child {
    translate: none;
  }
  .ticks span:last-child {
    translate: -100%;
  }
  .track {
    position: relative;
    width: 100%;
    height: var(--profile-track-height, 30px);
    border: 1px solid var(--border);
    border-radius: var(--r-well);
    background:
      linear-gradient(90deg, var(--border-subtle) 1px, transparent 1px) 0 0 /
        25% 100%,
      var(--surface-2);
    cursor: crosshair;
    isolation: isolate;
  }
  .track[aria-disabled="true"] {
    cursor: default;
  }
  .profile-artwork {
    position: absolute;
    inset: 0;
    overflow: clip;
    border-radius: inherit;
    pointer-events: none;
  }
  .range,
  .band,
  .position,
  .range-cap,
  .center-marker,
  .current-marker,
  .timeout-marker {
    position: absolute;
  }
  /* Static, full-width artwork moves on compositor transforms. Fixed-size
     marks have a full-width position wrapper so percentages use the track. */
  .range,
  .band,
  .position {
    left: 0;
    width: 100%;
    transform-origin: left center;
    pointer-events: none;
  }
  .position {
    inset-block: 0;
  }
  .lanes[data-motion="true"] :is(.range, .band, .position) {
    transition: transform var(--dur-graph) var(--ease-out);
  }
  .range {
    top: calc(50% - 2px);
    height: 5px;
    border-radius: var(--r-full);
    background: color-mix(in srgb, var(--text-soft) 40%, transparent);
  }
  .range-cap {
    left: 0;
    top: 18%;
    bottom: 18%;
    width: 1px;
    translate: -50%;
    background: color-mix(in srgb, var(--text-soft) 64%, transparent);
  }
  .band {
    top: 20%;
    height: 60%;
    border-radius: var(--r-full);
    background: color-mix(in srgb, var(--tone) 28%, transparent);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--tone) 30%, transparent);
  }
  .center-marker,
  .current-marker {
    left: 0;
    translate: -50%;
  }
  .center-marker {
    top: 5px;
    bottom: 5px;
    width: 2px;
    border-radius: var(--r-full);
    background: color-mix(in srgb, var(--text) 54%, transparent);
  }
  .current-marker {
    top: calc(50% - 5px);
    width: 10px;
    height: 10px;
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
    translate: -50%;
    background: color-mix(in srgb, var(--text) 54%, transparent);
    pointer-events: none;
  }
  .hover-card {
    z-index: 10;
    top: calc(50% - 12px);
    display: grid;
    gap: var(--space-1);
    min-width: 156px;
    max-width: min(238px, 76vw);
    translate: 0 -50%;
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
    color: var(--text-muted);
    font: var(--w-heavy) var(--type-2xs) var(--font-mono);
    font-style: normal;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .hover-head span {
    letter-spacing: var(--track-caps);
    text-transform: uppercase;
  }
  .hover-head strong {
    font: var(--w-heavy) var(--type-sm) var(--font-mono);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .hover-card > em {
    color: var(--err);
  }
  .lanes[data-variant="compact"] {
    --lane-pad: 10px;
  }
  .lanes[data-variant="compact"] .lane {
    padding-block: 9px 10px;
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
  .lanes[data-variant="compact"] .band {
    top: 4px;
    height: 14px;
  }
  .lanes[data-variant="compact"] .center-marker {
    top: 3px;
    bottom: 3px;
  }
  @container latency-lanes (max-width: 420px) {
    .jit,
    .range-label {
      display: none;
    }
  }
  @container latency-lanes (max-width: 300px) {
    .lanes[data-variant] .lane-meta strong {
      font-size: var(--type-xs);
    }
  }
</style>
