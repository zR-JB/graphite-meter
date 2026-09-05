<script lang="ts">
  // Live wrapper around the shared interactive latency distribution. Saved
  // results use the same LatencyProfileView implementation with finalized data.
  import { store } from "../state/store.svelte";
  import { failureDetail } from "./failurePresentation";
  import { LATENCY_LANES, type LatencyProfileViewLane } from "./latencyProfile";
  import LatencyProfileView from "./LatencyProfileView.svelte";

  const lanes = $derived<LatencyProfileViewLane[]>(
    LATENCY_LANES.filter(
      (meta) => store.stagePresentation[meta.key].configured,
    ).map((meta) => {
      const lane = store.latencyLanes.find((lane) => lane.key === meta.key)!;
      return {
        ...lane,
        current: store.isRunning ? lane.current : null,
        ...meta,
        tone: meta.key,
      };
    }),
  );
</script>

<section class="live-profile" aria-label="Latency distribution">
  {#if store.stagePresentation.latency.status === "failed"}
    <p class="lane-fail" role="alert">
      Latency skipped — {store.stagePresentation.latency.failure
        ? failureDetail(store.stageFailures.latency?.message)
        : "unavailable"}
    </p>
  {/if}

  <LatencyProfileView
    {lanes}
    variant="bare"
    showCurrent={store.isRunning}
    showTimeouts
  />
</section>

<style>
  .live-profile {
    --profile-track-height: clamp(32px, 3.5svh, 42px);
    --profile-lane-gap: 8px;
    width: 100%;
    overflow: visible;
  }
  .lane-fail {
    margin: 0 0 var(--space-2);
    padding: var(--space-1) var(--space-2);
    border: 1px solid color-mix(in srgb, var(--err) 40%, var(--border));
    border-radius: var(--r-chrome);
    background: var(--err-soft);
    color: var(--err);
    font-size: 11.5px;
    font-weight: 600;
    line-height: 1.35;
  }
</style>
