<script lang="ts">
  import { untrack } from "svelte";
  import { inView } from "../actions/inView";
  import { Smoothed } from "../presentation/motion.svelte";
  import Icon from "./Icon.svelte";
  import { tooltip } from "../actions/tooltip";
  import { JARGON, MISSING, STAGE } from "../presentation/vocabulary";
  import { fmtMs, formatLatency } from "../format";
  import { fmtGaugeTick } from "./gaugeScale";
  import {
    entries,
    probeAccountingDetails,
    hasProbeAccountingNotice,
    probeOutcomes,
    serverHandling,
    timeoutLabel,
    metricLabel,
    metricMeaning,
    metricValue,
    nearestMetric,
    pos,
    profileDomain,
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
  // Markers keep this margin, so one at either end of the scale stays whole.
  const EDGE = 7;
  const atPct = (pct: number, width: number) =>
    EDGE + (pct / 100) * (width - 2 * EDGE);
  const cardLeft = $derived.by(() => {
    if (!hover) return 0;
    const anchorPx = atPct(hover.anchorPct, hover.trackWidth);
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
      Math.max(0, (event.clientX - rect.left - EDGE) / (rect.width - 2 * EDGE)),
    );
    const metric = nearestMetric(lane, scale.min + ratio * scale.span);
    if (metric) setHover(lane, metric, track);
  }

  function onTrackFocus(event: FocusEvent, lane: LatencyProfileViewLane) {
    if (!(event.currentTarget as HTMLElement).matches(":focus-visible")) return;
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
      if (hover) event.preventDefault();
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

  // Markers glide between summaries on the shared frame clock; saved and unseen lanes snap.
  const GLIDED = ["min", "max", "p10", "p90", "center", "current"] as const;
  const glides = new Map<string, Smoothed>();
  $effect(() => {
    const snap = !motion;
    const targets = lanes.flatMap((lane) =>
      GLIDED.flatMap((metric) =>
        lane[metric] == null
          ? []
          : [
              [
                `${lane.key}:${metric}`,
                pos(lane[metric] ?? null, scale),
              ] as const,
            ],
      ),
    );
    untrack(() => {
      for (const [key, target] of targets) {
        const glide = glides.get(key) ?? new Smoothed();
        if (!glides.has(key)) glides.set(key, glide);
        if (glide.target !== target) glide.set(target, { snap });
      }
    });
  });
  const at = (lane: LatencyProfileViewLane, metric: (typeof GLIDED)[number]) =>
    glides.get(`${lane.key}:${metric}`)?.current ??
    pos(lane[metric] ?? null, scale);
  const spanTransform = (from: number, to: number) =>
    `translateX(${from}%) scaleX(${Math.max(0, to - from) / 100})`;
</script>

<div
  class="lanes"
  data-latency-profile
  {@attach live && inView((seen) => (motion = seen))}
  data-variant={variant}
  style:--edge={`${EDGE}px`}
  role="group"
  aria-label={label}
>
  {#each lanes as lane (lane.key)}
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
        <!-- The focused track's card explains these, so they stay out of the tab order. -->
        <strong
          tabindex="-1"
          {@attach tooltip(() =>
            [
              JARGON.latencyMedian,
              lane.reflectorTiming && serverHandling(lane.reflectorTiming),
            ]
              .filter(Boolean)
              .join("\n"),
          )}>median {formatLatency(lane.center)}</strong
        >
        <em class="jit" tabindex="-1" {@attach tooltip(() => JARGON.jitter)}
          >jitter {formatLatency(lane.jitter)}</em
        >
        <em
          class="range-label"
          tabindex="-1"
          {@attach tooltip(() => JARGON.latencyRange)}
        >
          range {lane.min == null || lane.max == null
            ? MISSING
            : `${fmtMs(lane.min)} – ${fmtMs(lane.max)} ms`}
        </em>
        <span class="accounting-slot">
          {#if hasProbeAccountingNotice(lane)}
            <span
              class="timing-info"
              role="note"
              aria-label={`${lane.label}: ${probeAccountingDetails(lane)}`}
              {@attach tooltip(() => probeOutcomes(lane))}
              ><Icon name="info" /></span
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
          ? `${metricLabel(hover.metric)} ${fmtMs(hoverValue)} milliseconds, ${metricMeaning(hover.metric)}`
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
              style:transform={spanTransform(at(lane, "min"), at(lane, "max"))}
            ></span>
            <span
              class="position"
              style:transform={`translateX(${at(lane, "min")}%)`}
              ><i class="range-cap"></i></span
            >
            <span
              class="position"
              style:transform={`translateX(${at(lane, "max")}%)`}
              ><i class="range-cap"></i></span
            >
          {/if}
          {#if lane.p10 != null && lane.p90 != null}
            <span
              class="band"
              style:transform={spanTransform(at(lane, "p10"), at(lane, "p90"))}
            ></span>
          {/if}
          {#if lane.center != null}
            <span
              class="position"
              style:transform={`translateX(${at(lane, "center")}%)`}
              ><i class="center-marker"></i></span
            >
          {/if}
          {#if live && lane.current != null}
            <span
              class="position"
              style:transform={`translateX(${at(lane, "current")}%)`}
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
          <span
            class="guide"
            style:left={`${atPct(pos(hoverValue, scale), hover.trackWidth)}px`}
          ></span>
          <span
            class="inspect-card hover-card"
            bind:clientWidth={cardWidth}
            style:left={`${cardLeft}px`}
          >
            <span class="hover-head">
              <span>{metricLabel(hover.metric)}</span>
              <strong>{fmtMs(hoverValue)} ms</strong>
            </span>
            <span class="hover-meaning">{metricMeaning(hover.metric)}</span>
          </span>
        {/if}
      </div>
    </div>
  {/each}
  <div class="ticks" aria-hidden="true">
    {#each ticks as tick, index (index)}
      <span style={`left:${pos(tick, scale)}%`}
        >{fmtGaugeTick(tick)}{index === 2 ? " ms" : ""}</span
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
    transition: var(--transition-control);
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
  /* Fixed widths keep changing numbers from moving the median; a narrow lane drops facts, never truncates them. */
  .lane-meta em {
    flex: none;
    min-width: 15ch;
    color: var(--text-muted);
    font: var(--w-normal) var(--type-2xs) var(--font-mono);
    font-variant-numeric: tabular-nums;
    text-align: end;
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
    color: var(--warn);
  }
  .timing-info :global(svg) {
    width: 12px;
    height: 12px;
  }
  .ticks {
    position: relative;
    height: 13px;
    margin-inline: calc(var(--lane-pad) + 1px + var(--edge));
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
    inset: 0 var(--edge);
    overflow: clip;
    overflow-clip-margin: var(--edge);
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
    top: 50%;
    display: grid;
    gap: 2px;
    min-width: 156px;
    max-width: min(238px, 76vw);
    padding-block: var(--space-1);
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
  .hover-meaning {
    overflow: hidden;
    color: var(--text-muted);
    font: var(--w-heavy) var(--type-2xs) var(--font-mono);
    font-style: normal;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .hover-meaning {
    font-weight: var(--w-normal);
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
  @container latency-lanes (max-width: 500px) {
    .range-label {
      display: none;
    }
  }
  @container latency-lanes (max-width: 400px) {
    .jit {
      display: none;
    }
  }
  @container latency-lanes (max-width: 300px) {
    .lanes[data-variant] .lane-meta strong {
      font-size: var(--type-xs);
    }
  }
</style>
