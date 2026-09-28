<script lang="ts">
  import { untrack } from "svelte";
  import { inView } from "../actions/inView";
  import { handoff, Smoothed } from "../presentation/motion.svelte";
  import Icon from "./Icon.svelte";
  import { term, tooltip } from "../actions/tooltip";
  import { warmUp } from "../actions/intent";
  import { scrub } from "../actions/scrub";
  import { JARGON, MISSING, STAGE } from "../presentation/vocabulary";
  import { fmtAddedMs, fmtMs, formatLatency } from "../format";
  import { fmtGaugeTick } from "./gaugeScale";
  import {
    entries,
    formatTimeouts,
    probeAccountingDetails,
    hasProbeAccountingNotice,
    probeOutcomes,
    serverHandling,
    timeoutsTip,
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
    /** Signed ms each load added over the idle median; set once the run has it. */
    added?: Partial<
      Record<LatencyProfileViewLane["key"], number | null>
    > | null;
    stability?: number | null;
    /** Whose latency this is, when several servers ran. */
    source?: string;
  }

  let {
    lanes,
    variant = "bare",
    label = "Latency, jitter and probe timeouts by phase",
    added = null,
    stability = null,
    source,
  }: Props = $props();
  const idle = $derived(lanes.find((lane) => lane.key === "latency") ?? null);

  let motion = $state(false);

  const scale = $derived(profileDomain(lanes));
  const live = $derived(variant === "bare");
  const ticks = handoff(
    () =>
      [0, scale / 2, scale].map((tick) => ({
        left: pos(tick, scale),
        text: fmtGaugeTick(tick),
      })),
    (ticks) => ticks.map((tick) => tick.text).join(),
  );

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

  // A card answers the pointer near a marker at once and follows it along the lane; a finger reaches further.
  function inspect(
    x: number,
    lane: LatencyProfileViewLane,
    track: HTMLElement,
    reach: number,
  ) {
    const rect = track.getBoundingClientRect();
    const ratio = Math.min(
      1,
      Math.max(0, (x - rect.left - EDGE) / (rect.width - 2 * EDGE)),
    );
    const metric = nearestMetric(lane, ratio * scale);
    const px =
      metric && atPct(pos(metricValue(lane, metric), scale), rect.width);
    if (metric && Math.abs(px! - (x - rect.left)) <= reach)
      setHover(lane, metric, track);
    else if (keyboardLane !== lane.key) hover = null;
  }
  const laneOf = (event: PointerEvent) => {
    const track = event.currentTarget as HTMLElement;
    return lanes.find((lane) => lane.key === track.dataset.lane) ?? null;
  };
  const pointer = scrub({
    read(event) {
      const lane = laneOf(event);
      if (lane)
        inspect(
          event.clientX,
          lane,
          event.currentTarget as HTMLElement,
          event.pointerType === "touch" ? 24 : 12,
        );
    },
    clear(event) {
      if (keyboardLane !== laneOf(event)?.key) hover = null;
    },
  });
  function onTrackLeave(event: PointerEvent, lane: LatencyProfileViewLane) {
    if (event.pointerType === "touch") return;
    if (hover) warmUp();
    if (keyboardLane !== lane.key) hover = null;
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

  function accessibleLane(
    lane: LatencyProfileViewLane,
    plus: number | null,
  ): string {
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
      plus == null ? null : `added over idle ${fmtAddedMs(plus)} milliseconds`,
      lane.timeoutRatio == null
        ? null
        : `${formatTimeouts(lane.timeoutRatio)} probe timeouts`,
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
      for (const key of glides.keys())
        if (!targets.some(([other]) => other === key)) glides.delete(key);
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
  // Marks sit on the track by their edges, never a transform, so outlines and radii stay on whole pixels.
  const span = (from: number, to: number) =>
    `left:${from}%;width:${Math.max(0, to - from)}%`;
</script>

<section class="latency-card" data-tone="latency" aria-label={label}>
  <header class="card-head">
    <span class="dot" aria-hidden="true"></span>
    <h3 {@attach tooltip(() => JARGON.latency)}>{STAGE.latency.label}</h3>
    <span class="aside"
      >{source ? `${source}, idle and under load` : "Idle and under load"}</span
    >
  </header>
  <div class="body">
    {#if idle}
      <div class="idle">
        <div class="headline" class:quiet={idle.center == null}>
          <span class="num"
            >{idle.center == null ? MISSING : fmtMs(idle.center)}</span
          >
          {#if idle.center != null}<span class="unit">ms</span>{/if}
        </div>
        <span class="caption">Idle median</span>
        <dl class="facts">
          <div>
            <dt {@attach term(() => JARGON.jitter)}>Jitter</dt>
            <dd>{formatLatency(idle.jitter)}</dd>
          </div>
          <div>
            <dt {@attach term(() => JARGON.latencyRange)}>Range</dt>
            <dd>
              {idle.min == null || idle.max == null
                ? MISSING
                : `${fmtMs(idle.min)}–${fmtMs(idle.max)} ms`}
            </dd>
          </div>
          {#if stability != null}
            <div>
              <dt>Stability</dt>
              <dd>{Math.round(stability)}%</dd>
            </div>
          {/if}
          <div>
            <dt>Timeouts</dt>
            <dd>{formatTimeouts(idle.timeoutRatio)}</dd>
          </div>
        </dl>
      </div>
    {/if}
    <div
      class="lanes"
      data-latency-profile
      {@attach live && inView((seen) => (motion = seen))}
      data-variant={variant}
      style:--edge={`${EDGE}px`}
      style:--lanes={lanes.length}
      role="group"
      aria-label="Median, jitter, timeouts and spread by stage"
    >
      <div class="lane-head" aria-hidden="true">
        <span></span><span>Median</span><span>Jitter</span><span>Timeouts</span
        ><span></span><span>Added</span>
      </div>
      {#each lanes as lane (lane.key)}
        {@const metrics = entries(lane)}
        {@const selected =
          hover?.key === lane.key
            ? metrics.findIndex((entry) => entry.metric === hover!.metric)
            : -1}
        {@const plus =
          lane.key === "latency" ? null : (added?.[lane.key] ?? null)}
        <div
          class="lane"
          data-tone={lane.key}
          data-active={lane.active === true}
        >
          <span class="lane-name">
            <span class="dot" aria-hidden="true"></span>
            <span class="lane-label">{lane.label}</span>
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
          <strong
            class="lane-median"
            tabindex="-1"
            {@attach tooltip(() =>
              [
                JARGON.latencyMedian,
                lane.reflectorTiming && serverHandling(lane.reflectorTiming),
              ]
                .filter(Boolean)
                .join("\n"),
            )}>{formatLatency(lane.center)}</strong
          >
          <em class="lane-jitter">{formatLatency(lane.jitter)}</em>
          <em
            class="lane-timeouts"
            tabindex="-1"
            {@attach tooltip(() => timeoutsTip(lane))}
            >{formatTimeouts(lane.timeoutRatio)}</em
          >
          <div
            class="track"
            role="slider"
            tabindex={metrics.length ? 0 : -1}
            aria-label={accessibleLane(lane, plus)}
            aria-disabled={!metrics.length}
            aria-valuemin={0}
            aria-valuemax={Math.max(0, metrics.length - 1)}
            aria-valuenow={Math.max(0, selected)}
            aria-valuetext={selected >= 0 && hover && hoverValue != null
              ? `${metricLabel(hover.metric)} ${fmtMs(hoverValue)} milliseconds, ${metricMeaning(hover.metric)}`
              : undefined}
            data-lane={lane.key}
            onpointerdown={pointer.down}
            onpointermove={pointer.move}
            onpointerup={pointer.up}
            onpointercancel={pointer.cancel}
            onpointerleave={(event) => onTrackLeave(event, lane)}
            onfocus={(event) => onTrackFocus(event, lane)}
            onblur={() => {
              keyboardLane = null;
              hover = null;
            }}
            onkeydown={(event) => onTrackKey(event, lane)}
          >
            <span class="profile-artwork" aria-hidden="true">
              {#if idle?.center != null && lanes.length > 1}
                <i class="baseline" style:left={`${at(idle, "center")}%`}></i>
                {#if lane.center != null && lane.center > idle.center}
                  <i
                    class="added-span"
                    style={span(at(idle, "center"), at(lane, "center"))}
                  ></i>
                {/if}
              {/if}
              {#if lane.min != null && lane.max != null}
                <i class="range" style={span(at(lane, "min"), at(lane, "max"))}
                ></i>
              {/if}
              {#if lane.p10 != null && lane.p90 != null}
                <i class="band" style={span(at(lane, "p10"), at(lane, "p90"))}
                ></i>
              {/if}
              {#if lane.center != null}
                <i class="center-marker" style:left={`${at(lane, "center")}%`}
                ></i>
              {/if}
              {#if live && lane.current != null}
                <i class="current-marker" style:left={`${at(lane, "current")}%`}
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
          <span class="lane-added" class:baseline-word={lane.key === "latency"}
            >{lane.key === "latency"
              ? "Baseline"
              : plus == null
                ? ""
                : `${fmtAddedMs(plus)} ms`}</span
          >
        </div>
      {/each}
      <div class="ticks" aria-hidden="true" style:opacity={ticks.opacity}>
        {#each ticks.shown as tick, index (index)}
          <span style={`left:${tick.left}%`}
            >{tick.text}{index === 2 ? " ms" : ""}</span
          >
        {/each}
      </div>
    </div>
  </div>
</section>

<style>
  /* Latency is the stage's light on the page like every card: a rule in its hue and a wash.
     The card is as tall as its table, so the wash fades only partway and still marks where it ends. */
  .latency-card {
    --wash: 7%;
    display: grid;
    gap: var(--space-3);
    min-width: 0;
    padding: var(--space-3) var(--space-4) var(--space-3);
    border-top: 2px solid var(--tone);
    background: linear-gradient(
      180deg,
      color-mix(in oklab, var(--tone) var(--wash), transparent),
      color-mix(in oklab, var(--tone) calc(var(--wash) / 3), transparent)
    );
    container: latency / inline-size;
  }
  .card-head {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
    min-height: 20px;
  }
  .dot {
    flex: none;
    width: 7px;
    height: 7px;
    border-radius: var(--r-full);
    background: var(--tone);
  }
  h3 {
    font: var(--w-strong) var(--type-md) / 20px var(--font-sans);
    white-space: nowrap;
  }
  .aside {
    min-width: 0;
    overflow: hidden;
    color: var(--text-soft);
    font: var(--w-normal) var(--type-sm) / 1 var(--font-sans);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .body {
    display: grid;
    grid-template-columns: minmax(176px, 0.62fr) minmax(0, 2fr);
    gap: var(--space-5);
  }
  .idle {
    display: grid;
    align-self: center;
    align-content: start;
    gap: 2px;
    min-width: 0;
  }
  .headline {
    display: flex;
    align-items: baseline;
    gap: 8px;
    white-space: nowrap;
  }
  .num {
    font: 300 clamp(32px, 2.6vw, 46px) / 1 var(--font-display);
    font-variant-numeric: tabular-nums;
    letter-spacing: -0.025em;
  }
  .quiet .num {
    color: var(--text-soft);
  }
  .unit {
    color: var(--text-muted);
    font: var(--w-normal) var(--type-md) / 1 var(--font-sans);
  }
  .caption {
    color: var(--text-soft);
    font: var(--w-normal) var(--type-sm) / 1.4 var(--font-sans);
  }
  .facts {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: var(--space-2) var(--space-3);
    margin-top: var(--space-3);
    padding-top: var(--space-2);
    border-top: var(--hairline) solid var(--border-subtle);
  }
  .facts > div {
    display: grid;
    gap: 1px;
    min-width: 0;
  }
  .facts dt {
    width: fit-content;
    color: var(--text-soft);
    font: var(--w-normal) var(--type-sm) / 1.3 var(--font-sans);
  }
  .facts dd {
    font: var(--w-normal) var(--type-md) / 1.3 var(--font-sans);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  /* One row per population on one scale: name, median, jitter, timeouts, spread, and what the load added.
     Rows abut, so the scale's gridlines and the idle median run through them as single lines;
     the block stays whole and centred beside a taller idle column. */
  .lanes {
    display: grid;
    align-self: center;
    grid-template-columns:
      repeat(4, max-content) [track] minmax(120px, 1fr)
      minmax(64px, max-content);
    grid-template-rows: auto repeat(var(--lanes), 40px) auto;
    column-gap: var(--space-3);
    min-width: 0;
    isolation: isolate;
  }
  .lane-head,
  .lane {
    display: grid;
    grid-column: 1 / -1;
    grid-template-columns: subgrid;
    align-items: center;
  }
  .lane-head {
    padding-bottom: var(--space-1);
    color: var(--text-soft);
    font: var(--w-normal) var(--type-xs) / 1 var(--font-sans);
    text-align: end;
  }
  .lane-name {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
    color: var(--text-muted);
    font: var(--w-normal) var(--type-body) / 1 var(--font-sans);
    white-space: nowrap;
  }
  .lane-name .dot {
    width: 6px;
    height: 6px;
  }
  .lane-median {
    font: var(--w-normal) var(--type-md) / 1 var(--font-sans);
    font-variant-numeric: tabular-nums;
    text-align: end;
    white-space: nowrap;
  }
  .lane-jitter,
  .lane-timeouts {
    color: var(--text-muted);
    font: var(--w-normal) var(--type-sm) / 1 var(--font-sans);
    font-style: normal;
    font-variant-numeric: tabular-nums;
    text-align: end;
    white-space: nowrap;
  }
  .lane-added {
    color: var(--tone-ink);
    font: var(--w-strong) var(--type-body) / 1 var(--font-sans);
    font-variant-numeric: tabular-nums;
    text-align: end;
    white-space: nowrap;
  }
  .lane-added.baseline-word {
    color: var(--text-soft);
    font-weight: var(--w-normal);
  }
  .timing-info {
    display: inline-grid;
    place-items: center;
    width: 18px;
    height: 18px;
    color: var(--warn);
  }
  .timing-info :global(svg) {
    width: 12px;
    height: 12px;
  }
  .ticks {
    position: relative;
    grid-column: track;
    height: 16px;
    margin-inline: var(--edge);
    border-top: var(--hairline) solid var(--border-subtle);
  }
  .ticks span {
    position: absolute;
    top: 4px;
    translate: -50%;
    color: var(--text-soft);
    font: var(--type-2xs) / 1.2 var(--font-sans);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .ticks span:first-child {
    translate: none;
  }
  .ticks span:last-child {
    translate: -100%;
  }
  /* A track fills its row, so the pointer reads anywhere on it and its lines meet the next row's. */
  .track {
    position: relative;
    align-self: stretch;
    cursor: crosshair;
    touch-action: pan-y pinch-zoom;
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
  .profile-artwork > i,
  .profile-artwork::before,
  .profile-artwork::after {
    position: absolute;
  }
  /* The axis ticks run up through the rows as gridlines, behind the plots: 0 and the top at the edges. */
  .profile-artwork::before,
  .profile-artwork::after {
    content: "";
    z-index: -1;
    inset-block: 0;
    border-left: var(--hairline) solid var(--border-subtle);
  }
  .profile-artwork::before {
    inset-inline: 0;
    border-right: var(--hairline) solid var(--border-subtle);
  }
  .profile-artwork::after {
    left: 50%;
  }
  /* The idle median drops from its own tick through every loaded row. */
  .baseline {
    inset-block: 0;
    width: 1px;
    margin-left: -0.5px;
    background: color-mix(in oklab, var(--phase-latency) 60%, transparent);
  }
  .lane[data-tone="latency"] .baseline {
    top: 50%;
  }
  /* What the load added: from the idle median to this population's. */
  .added-span {
    top: calc(50% - 1px);
    height: 2px;
    background: color-mix(in oklab, var(--tone) 50%, transparent);
  }
  .range {
    top: calc(50% - 0.5px);
    height: 1px;
    background: color-mix(in oklab, var(--tone) 45%, transparent);
  }
  .band {
    top: calc(50% - 6px);
    height: 12px;
    border-radius: var(--r-well);
    background: color-mix(in oklab, var(--tone) 22%, transparent);
    box-shadow: inset 0 0 0 1px
      color-mix(in oklab, var(--tone) 34%, transparent);
  }
  .center-marker {
    top: calc(50% - 10px);
    width: 2px;
    height: 20px;
    margin-left: -1px;
    border-radius: 1px;
    background: var(--tone);
  }
  .current-marker {
    top: calc(50% - 4px);
    width: 8px;
    height: 8px;
    margin-left: -4px;
    border-radius: var(--r-full);
    background: var(--tone);
  }
  .guide {
    position: absolute;
    z-index: 4;
    inset-block: 0;
    width: 1px;
    margin-left: -0.5px;
    background: color-mix(in srgb, var(--text) 54%, transparent);
    pointer-events: none;
  }
  /* Centred by auto margins, not a translate, so its text stays on whole pixels. */
  .hover-card {
    z-index: 10;
    inset-block: 0;
    display: grid;
    gap: 2px;
    height: fit-content;
    min-width: 156px;
    max-width: min(238px, 76vw);
    margin-block: auto;
    padding-block: var(--space-1);
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
    color: var(--text-soft);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .hover-head strong {
    font: var(--w-strong) var(--type-sm) var(--font-sans);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  /* Narrower cards put the idle facts above the rows. */
  @container latency (max-width: 720px) {
    .body {
      grid-template-columns: minmax(0, 1fr);
      gap: var(--space-3);
    }
    .idle {
      grid-template-columns: auto 1fr;
      align-items: end;
      column-gap: var(--space-4);
    }
    .idle .caption {
      grid-row: 2;
    }
    .idle .facts {
      grid-column: 2;
      grid-row: 1 / 3;
      grid-template-columns: repeat(4, max-content);
      margin: 0;
      padding: 0;
      border: 0;
    }
  }
  @container latency (max-width: 480px) {
    .idle {
      grid-template-columns: minmax(0, 1fr);
    }
    .idle .facts {
      grid-column: 1;
      grid-row: auto;
      grid-template-columns: repeat(2, minmax(0, 1fr));
      margin-top: var(--space-2);
    }
  }
  /* Narrow: jitter leaves the row for the hover; the timeouts stay. */
  @container latency (max-width: 560px) {
    .lanes {
      grid-template-columns:
        repeat(3, max-content) [track] minmax(80px, 1fr)
        max-content;
    }
    .lane-jitter,
    .lane-head > :nth-child(3) {
      display: none;
    }
  }
  /* A phone gives each population two lines: its numbers, then its plot at full width. */
  @container latency (max-width: 440px) {
    .lanes {
      grid-template-columns: minmax(0, 1fr) repeat(3, max-content);
      grid-template-rows: none;
    }
    .lane {
      row-gap: 2px;
      padding-top: var(--space-2);
    }
    /* On the smallest phones a name wraps before it runs into the figures. */
    .lane-name {
      white-space: normal;
    }
    .lane-head > :nth-child(5) {
      display: none;
    }
    .track {
      grid-column: 1 / -1;
      grid-row: 2;
      height: 28px;
    }
    .ticks {
      grid-column: 1 / -1;
    }
  }
</style>
