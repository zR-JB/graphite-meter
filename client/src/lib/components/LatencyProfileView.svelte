<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import { inView } from "../actions/inView";
  import { handoff, Smoothed } from "../presentation/motion.svelte";
  import Icon from "./Icon.svelte";
  import { termAction, tooltipAction } from "../actions/tooltip";
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
    /** Signed ms each load added over the idle median, once the run has it; until then the medians give it. */
    added?: Partial<
      Record<LatencyProfileViewLane["key"], number | null>
    > | null;
    stability?: number | null;
    /** Whose latency this is, when several servers ran. */
    source?: string;
    /** Why the Latency stage failed; it stands under the idle headline in place of its caption. */
    failure?: string;
  }

  let {
    lanes,
    variant = "bare",
    label = "Latency, jitter and probe timeouts by phase",
    added = null,
    stability = null,
    source,
    failure,
  }: Props = $props();
  const idle = $derived(lanes.find((lane) => lane.key === "latency") ?? null);
  // Lit like a stage card: dim until something is measured, brightest while the idle stage runs.
  const light = $derived(
    idle?.active
      ? "active"
      : lanes.some((lane) => lane.center != null)
        ? "complete"
        : "pending",
  );
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

  let hover = $state.raw<{
    key: LatencyProfileViewLane["key"];
    metric: MetricKey;
    anchorPct: number;
    trackWidth: number;
  } | null>(null);
  let keyboardLane = $state<LatencyProfileViewLane["key"] | null>(null);

  const hoverLane = $derived(
    hover ? (lanes.find((lane) => lane.key === hover!.key) ?? null) : null,
  );
  const hoverValue = $derived(
    hoverLane && hover ? metricValue(hoverLane, hover.metric) : null,
  );

  // Markers keep this margin, so one at either end of the scale stays whole.
  const EDGE = 7;
  const atPct = (pct: number, width: number) =>
    EDGE + (pct / 100) * (width - 2 * EDGE);

  function setHover(
    lane: LatencyProfileViewLane,
    metric: MetricKey,
    track: HTMLElement,
    trackWidth = track.getBoundingClientRect().width,
  ) {
    const value = metricValue(lane, metric);
    if (value == null) return;
    const anchorPct = pos(value, scale);
    if (
      hover?.key === lane.key &&
      hover.metric === metric &&
      hover.anchorPct === anchorPct &&
      hover.trackWidth === trackWidth
    )
      return;
    hover = {
      key: lane.key,
      metric,
      anchorPct,
      trackWidth,
    };
  }

  // A card answers the pointer anywhere on the lane, like a stage graph's readout: it names the marker
  // nearest the pointer and follows the pointer from marker to marker, so a hand need not find a 2 px tick.
  function inspect(
    x: number,
    lane: LatencyProfileViewLane,
    track: HTMLElement,
  ) {
    const rect = track.getBoundingClientRect();
    const ratio = Math.min(
      1,
      Math.max(0, (x - rect.left - EDGE) / (rect.width - 2 * EDGE)),
    );
    const metric = nearestMetric(lane, ratio * scale);
    if (metric) setHover(lane, metric, track, rect.width);
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
        inspect(event.clientX, lane, event.currentTarget as HTMLElement);
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
  onDestroy(() => {
    for (const glide of glides.values()) glide.dispose();
  });
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
        if (!targets.some(([other]) => other === key)) {
          glides.get(key)?.dispose();
          glides.delete(key);
        }
      for (const [key, target] of targets) {
        const glide = glides.get(key) ?? new Smoothed();
        if (!glides.has(key)) glides.set(key, glide);
        if (snap || glide.target !== target) glide.set(target, { snap });
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

<section class="latency-card {light}" data-tone="latency" aria-label={label}>
  <header class="card-head">
    <span class="tone-icon" aria-hidden="true"
      ><Icon name={STAGE.latency.icon} /></span
    >
    <h3 use:tooltipAction={JARGON.latency}>
      {STAGE.latency.label}
    </h3>
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
        <span class="sub" class:failure={failure && idle.center == null}
          >{failure && idle.center == null ? failure : "Idle median"}</span
        >
        <dl class="facts">
          <div>
            <dt use:termAction={JARGON.jitter}>Jitter</dt>
            <dd class:quiet={idle.jitter == null}>
              {formatLatency(idle.jitter)}
            </dd>
          </div>
          <div>
            <dt use:termAction={JARGON.latencyRange}>Range</dt>
            <dd class:quiet={idle.min == null || idle.max == null}>
              {idle.min == null || idle.max == null
                ? MISSING
                : `${fmtMs(idle.min)}–${fmtMs(idle.max)} ms`}
            </dd>
          </div>
          <!-- Held from Start, so the result lands without moving Timeouts. -->
          <div>
            <dt use:tooltipAction={JARGON.latencyStability}>Stability</dt>
            <dd class:quiet={stability == null}>
              {stability == null ? MISSING : `${Math.round(stability)}%`}
            </dd>
          </div>
          <div>
            <dt use:tooltipAction={timeoutsTip(idle)}>Timeouts</dt>
            <dd class:quiet={idle.timeoutRatio == null}>
              {formatTimeouts(idle.timeoutRatio)}
            </dd>
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
          lane.key === "latency"
            ? null
            : (added?.[lane.key] ??
              (lane.center == null || idle?.center == null
                ? null
                : lane.center - idle.center))}
        {@const note = [
          lane.failure && `${lane.label}: ${lane.failure}`,
          hasProbeAccountingNotice(lane) && probeOutcomes(lane),
        ]
          .filter(Boolean)
          .join("\n")}
        <div
          class="lane"
          data-tone={lane.key}
          data-active={lane.active === true}
        >
          <span class="lane-name">
            <!-- A note takes the dot's place, so it never widens the column mid-run. -->
            {#if note}
              <span
                class="mark note"
                data-tone={lane.failure ? "err" : "warn"}
                role="note"
                aria-label={note.replaceAll("\n", ". ")}
                use:tooltipAction={note}><Icon name="info" /></span
              >
            {:else}
              <span class="mark tone-icon" aria-hidden="true"
                ><Icon name={STAGE[lane.key].icon} /></span
              >
            {/if}
            <span class="lane-label">{lane.label}</span>
          </span>
          <strong
            class="lane-median"
            class:quiet={lane.center == null}
            tabindex="-1"
            use:tooltipAction={[
              JARGON.latencyMedian,
              lane.reflectorTiming && serverHandling(lane.reflectorTiming),
            ]
              .filter(Boolean)
              .join("\n")}>{formatLatency(lane.center)}</strong
          >
          <em class="lane-jitter" class:quiet={lane.jitter == null}
            >{formatLatency(lane.jitter)}</em
          >
          <em
            class="lane-timeouts"
            class:quiet={lane.timeoutRatio == null}
            tabindex="-1"
            use:tooltipAction={timeoutsTip(lane)}
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
              {#if lane.min != null && lane.max != null}
                <i class="range" style={span(at(lane, "min"), at(lane, "max"))}
                ></i>
                <!-- At the whisker's own end, so the two glide together across a rescale. -->
                {#if lane.max > scale}
                  <i
                    class="overflow"
                    style:left={`${at(lane, "max")}%`}
                    data-max="{fmtMs(lane.max)} ms"
                  ></i>
                {/if}
              {/if}
              {#if lane.p10 != null && lane.p90 != null}
                <i class="band" style={span(at(lane, "p10"), at(lane, "p90"))}
                ></i>
              {/if}
              <!-- Over the boxes: the idle median runs through every row, and each load's span starts on it. -->
              {#if idle?.center != null && lanes.length > 1}
                <i class="baseline" style:left={`${at(idle, "center")}%`}></i>
                {#if lane.center != null && lane.center > idle.center}
                  <i
                    class="added-span"
                    style={span(at(idle, "center"), at(lane, "center"))}
                  ></i>
                {/if}
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
            {/if}
          </div>
          <span
            class="lane-added"
            class:quiet={plus == null}
            tabindex="-1"
            use:tooltipAction={JARGON.addedLatency}
            >{lane.key === "latency"
              ? "Baseline"
              : plus == null
                ? MISSING
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
      <!-- The marker under the pointer reads in a line of its own under the axis, never over a plot. -->
      <div class="reading" aria-hidden="true">
        {#if hoverLane && hover && hoverValue != null}
          <span class="reading-lane" data-tone={hoverLane.key}
            >{hoverLane.label}</span
          >
          <span class="reading-metric">{metricLabel(hover.metric)}</span>
          <strong>{fmtMs(hoverValue)} ms</strong>
          <span class="reading-meaning">{metricMeaning(hover.metric)}</span>
        {/if}
      </div>
    </div>
  </div>
</section>

<style>
  /* In its panel: the head at the top, then the idle figures beside the ruled lanes, centred in what is left. */
  .latency-card {
    display: grid;
    grid-template-rows: auto minmax(0, 1fr);
    gap: var(--space-3);
    min-width: 0;
    height: 100%;
    container: latency / inline-size;
  }
  .card-head {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
    min-height: 20px;
  }
  h3 {
    color: var(--text);
    font: var(--role-title);
    line-height: 22px;
    white-space: nowrap;
  }
  .card-head .tone-icon {
    width: 18px;
    height: 18px;
  }
  .card-head .tone-icon :global(svg) {
    width: 10px;
    height: 10px;
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
    align-self: center;
    grid-template-columns: minmax(176px, auto) minmax(0, 1fr);
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
    font: var(--role-readout);
    font-variant-numeric: tabular-nums;
  }
  .quiet .num {
    color: var(--text-soft);
  }
  .unit {
    color: var(--text-muted);
    font: 500 var(--type-sm) / 1 var(--font-mono);
  }
  .sub {
    color: var(--text-soft);
    font: var(--w-normal) var(--type-sm) / 1.4 var(--font-sans);
  }
  .sub.failure {
    color: var(--err);
    font-weight: var(--w-strong);
  }
  /* The idle figures are ruled rows under the headline, a quiet label and its figure on one line. */
  .facts {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    margin-top: var(--space-3);
    border-top: var(--hairline) solid var(--border-subtle);
  }
  .facts > div {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: var(--space-3);
    height: 24px;
    min-width: 0;
  }
  .facts > div + div {
    border-top: var(--hairline) solid var(--border-subtle);
  }
  .facts dt {
    width: fit-content;
    color: var(--text-soft);
    font: var(--role-label);
    line-height: 1;
  }
  .facts dd {
    font: var(--role-figure-sm);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  /* "—" for a value not yet measured is soft; a measured value is full ink. */
  .facts dd.quiet {
    color: var(--text-soft);
  }

  /* One row per population on one scale: name, median, jitter, timeouts, spread, and what the load added.
     Rows abut, so the scale's gridlines and the idle median run through them as single lines;
     the block stays whole and centred beside a taller idle column. */
  .lanes {
    display: grid;
    align-self: center;
    max-height: 100%;
    grid-template-columns:
      repeat(4, max-content) [track] minmax(120px, 1fr)
      minmax(64px, max-content);
    grid-template-rows: auto repeat(var(--lanes), minmax(36px, 52px)) auto auto;
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
  /* Rows are ruled, so the table reads as a grid with the axis ticks running up through it. */
  .lane + .lane {
    border-top: var(--hairline) solid var(--border-subtle);
  }
  /* Every figure in a row on the median's baseline (app.css, --role-label). */
  .lane-name {
    margin-top: calc(var(--type-md) - var(--type-body));
  }
  .lane-jitter,
  .lane-timeouts,
  .lane-added {
    margin-top: calc(var(--type-md) - var(--type-sm));
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
  /* One slot per row for its dot or a note, so a note arriving never moves the columns. */
  .mark {
    width: 18px;
    height: 18px;
  }
  .mark :global(svg) {
    width: 10px;
    height: 10px;
  }
  /* Figures take their longest value's width ("9999 ms") from Start, so arriving values never shift the plot. */
  .lane-median {
    min-width: 7ch;
    font: 500 var(--type-md) / 1 var(--font-mono);
    font-variant-numeric: tabular-nums;
    text-align: end;
    white-space: nowrap;
  }
  .lane-jitter {
    min-width: 7ch;
  }
  .lane-jitter,
  .lane-timeouts {
    color: var(--text-muted);
    font: 500 var(--type-sm) / 1 var(--font-mono);
    font-style: normal;
    font-variant-numeric: tabular-nums;
    text-align: end;
    white-space: nowrap;
  }
  .lane-median.quiet,
  .lane-jitter.quiet,
  .lane-timeouts.quiet {
    color: var(--text-soft);
  }
  .lane-added {
    color: var(--tone-ink);
    font: 600 var(--type-sm) / 1 var(--font-mono);
    font-variant-numeric: tabular-nums;
    text-align: end;
    white-space: nowrap;
  }
  .lane-added.quiet {
    color: var(--text-soft);
    font-weight: var(--w-normal);
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
    font: 500 var(--type-2xs) / 1.2 var(--font-mono);
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
    border-radius: var(--r-well);
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
  .range,
  .overflow {
    top: calc(50% - 0.5px);
    height: 1px;
    background: color-mix(in oklab, var(--tone) 45%, transparent);
  }
  /* A reply past the axis: its whisker runs on to the edge, ends in an arrowhead and names its value. */
  .overflow {
    width: var(--edge);
  }
  .overflow::before,
  .overflow::after {
    position: absolute;
    right: 0;
  }
  /* The value ends inside the axis, clear of its last gridline. */
  .overflow::before {
    content: attr(data-max);
    right: calc(var(--edge) + var(--space-1));
    bottom: var(--space-1);
    color: var(--text-soft);
    font: 500 var(--type-2xs) / 1 var(--font-mono);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .overflow::after {
    content: "";
    top: -3px;
    border-left: 5px solid var(--tone);
    border-block: 3.5px solid transparent;
  }
  /* Opaque, so its whisker and the gridlines stay behind it. */
  .band {
    top: calc(50% - 6px);
    height: 12px;
    border-radius: var(--r-well);
    background: color-mix(in oklab, var(--tone) 22%, var(--canvas));
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
  /* One line kept under the axis from Start, so a reading arriving never moves the rows. */
  .reading {
    display: flex;
    grid-column: 1 / -1;
    align-items: baseline;
    gap: var(--space-2);
    height: 20px;
    min-width: 0;
    margin-top: var(--space-1);
    overflow: hidden;
    color: var(--text-soft);
    font: var(--w-normal) var(--type-sm) / 20px var(--font-sans);
    white-space: nowrap;
  }
  .reading-lane {
    color: var(--tone-ink);
    font-weight: var(--w-strong);
  }
  .reading strong {
    color: var(--text);
    font: var(--role-figure-sm);
    line-height: 20px;
  }
  .reading-meaning {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
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
    .idle .sub {
      grid-row: 2;
    }
    .idle .facts {
      grid-column: 2;
      grid-row: 1 / 3;
      grid-template-columns: repeat(4, max-content);
      gap: 0 var(--space-4);
      margin: 0;
      padding: 0;
      border: 0;
    }
    .idle .facts > div,
    .idle .facts > div + div {
      display: grid;
      gap: 4px;
      height: auto;
      border: 0;
    }
  }
  @container latency (max-width: 400px) {
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
    .lane-median {
      min-width: 0;
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
    /* Figures sit above a phone's plot, so an overflow names its value under the whisker. */
    .overflow::before {
      top: var(--space-1);
      bottom: auto;
    }
  }
</style>
