<script lang="ts">
  import { latencyScale } from "../presentation/scales";
  import { stageGraph, type LatencyPoint } from "../presentation/stageGraph";

  // Replies over a stage as bars from the idle median: the stage graph's latency track at a strip's height.
  let {
    points,
    start,
    span,
    baseline,
  }: {
    points: LatencyPoint[];
    start: number;
    span: number;
    baseline: number | null;
  } = $props();
  let width = $state(0);
  let height = $state(0);
  const drawn = $derived(
    width && height
      ? stageGraph({
          lanes: [],
          latency: points,
          start,
          span,
          ceiling: 1,
          baseline,
          latencyTop: latencyScale(points.map((point) => point.ms)),
          width,
          plotHeight: 0,
          trackHeight: height,
        })
      : null,
  );
</script>

<div
  class="trace"
  aria-hidden="true"
  bind:clientWidth={width}
  bind:clientHeight={height}
>
  {#if drawn}
    <svg {width} {height}>
      {#if drawn.baselineY !== null}<line
          class="reply-median"
          x1="0"
          x2={width}
          y1={drawn.baselineY}
          y2={drawn.baselineY}
        />{/if}
      {#each drawn.dots as dot, index (index)}
        <line
          class="reply"
          x1={dot.x}
          x2={dot.x}
          y1={drawn.baselineY ?? height}
          y2={dot.y}
        />
      {/each}
    </svg>
  {/if}
</div>

<style>
  .trace {
    position: relative;
    height: 100%;
    min-height: 0;
  }
  svg {
    position: absolute;
    inset: 0;
    overflow: visible;
  }
</style>
