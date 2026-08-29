<script lang="ts">
  import { formatLatency, formatPercent } from "../../history/format";
  import type {
    LatencyLaneSnapshot,
    LatencySnapshot,
  } from "../../history/types";
  import { profileDomain } from "../latencyProfile";
  import LatencyProfileTrack from "../LatencyProfileTrack.svelte";

  export interface FinalizedLatencyLane extends LatencyLaneSnapshot {
    key: "latency" | "download" | "upload" | "bidirectional";
    label: string;
    icon: string;
    headline?: Pick<LatencySnapshot, "p50Ms" | "p95Ms" | "stabilityScore">;
  }

  interface Props {
    lanes: FinalizedLatencyLane[];
    showLoss?: boolean;
  }
  let { lanes, showLoss = false }: Props = $props();
  const useful = $derived(
    lanes.filter((lane) =>
      [lane.min, lane.max, lane.p10, lane.p90, lane.center, lane.jitter].some(
        (value) => value != null,
      ),
    ),
  );
  const domain = $derived(profileDomain(useful));
</script>

{#if useful.length}
  <div class="latency-profile-summary" aria-label="Latency distributions">
    {#each useful as lane (lane.key)}
      <article class="profile-lane" data-tone={lane.key}>
        <header>
          <span class="lane-name">
            <i>{@html lane.icon}</i><strong>{lane.label}</strong>
          </span>
          {#if lane.center != null}<b>{formatLatency(lane.center)}</b>{/if}
        </header>
        <div class="profile-track" aria-hidden="true">
          <LatencyProfileTrack
            {lane}
            {domain}
            {showLoss}
            tone={lane.key}
            compact
          />
        </div>
        <dl>
          {#if lane.min != null}<div>
              <dt>Min</dt>
              <dd>{formatLatency(lane.min)}</dd>
            </div>{/if}
          {#if lane.p10 != null && lane.p90 != null}<div>
              <dt>P10–P90</dt>
              <dd>{formatLatency(lane.p10)}–{formatLatency(lane.p90)}</dd>
            </div>{/if}
          {#if lane.max != null}<div>
              <dt>Max</dt>
              <dd>{formatLatency(lane.max)}</dd>
            </div>{/if}
          {#if lane.jitter != null}<div>
              <dt>Jitter</dt>
              <dd>{formatLatency(lane.jitter)}</dd>
            </div>{/if}
          {#if lane.headline}
            <div>
              <dt>P50</dt>
              <dd>{formatLatency(lane.headline.p50Ms)}</dd>
            </div>
            <div>
              <dt>P95</dt>
              <dd>{formatLatency(lane.headline.p95Ms)}</dd>
            </div>
            <div>
              <dt>Stability</dt>
              <dd>{formatPercent(lane.headline.stabilityScore * 100, 0)}</dd>
            </div>
          {/if}
          {#if showLoss && lane.lossRatio > 0}<div>
              <dt>Loss</dt>
              <dd>{formatPercent(lane.lossRatio * 100)}</dd>
            </div>{/if}
          <div>
            <dt>Samples</dt>
            <dd>{lane.count}</dd>
          </div>
        </dl>
      </article>
    {/each}
  </div>
{:else}
  <p class="not-run">No useful latency lanes were retained.</p>
{/if}

<style>
  .latency-profile-summary {
    display: grid;
    gap: var(--space-2);
  }
  .profile-lane {
    --tone: var(--phase-latency);
    display: grid;
    gap: 8px;
    padding: 10px 12px;
    border-left: 2px solid var(--tone);
    background: linear-gradient(
      112deg,
      color-mix(in srgb, var(--tone) 9%, var(--surface-2)),
      var(--surface-1) 76%
    );
    box-shadow: var(--elev-recess);
  }
  .profile-lane[data-tone="download"] {
    --tone: var(--phase-download);
  }
  .profile-lane[data-tone="upload"] {
    --tone: var(--phase-upload);
  }
  .profile-lane[data-tone="bidirectional"] {
    --tone: var(--phase-bidirectional);
  }
  header,
  .lane-name {
    display: flex;
    align-items: center;
    gap: 7px;
    min-width: 0;
  }
  header {
    justify-content: space-between;
  }
  .lane-name {
    color: var(--text-muted);
    font-size: var(--type-xs);
  }
  .lane-name i {
    display: grid;
    color: var(--tone);
    font-style: normal;
  }
  .lane-name :global(svg) {
    width: 15px;
    height: 15px;
  }
  header b {
    color: var(--text);
    font: 650 var(--type-sm) var(--font-mono);
  }
  .profile-track {
    position: relative;
    height: 18px;
    border: 1px solid var(--border);
    border-radius: var(--r-full);
    background: var(--surface-2);
  }
  dl {
    display: flex;
    flex-wrap: wrap;
    gap: 5px 14px;
    margin: 0;
  }
  dl div {
    display: grid;
    gap: 1px;
  }
  dt {
    color: var(--text-soft);
    font-size: 9px;
  }
  dd {
    margin: 0;
    color: var(--text-muted);
    font: 600 10px var(--font-mono);
  }
  .not-run {
    margin: 0;
    padding: 12px;
    color: var(--text-soft);
    font-size: var(--type-xs);
  }
</style>
