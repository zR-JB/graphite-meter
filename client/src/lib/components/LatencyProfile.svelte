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
        ...meta,
        tone: meta.key,
      };
    }),
  );
</script>

<section class="live-profile" aria-label="Latency distribution">
  {#if store.stagePresentation.latency.status === "failed"}
    <p class="notice" data-tone="err" role="alert">
      Latency skipped — {store.stagePresentation.latency.failure
        ? failureDetail(store.stageFailures.latency?.message)
        : "unavailable"}
    </p>
  {/if}

  <LatencyProfileView {lanes} variant="bare" showCurrent showTimeouts />
</section>

<style>
  .live-profile {
    --profile-track-height: clamp(32px, 3.5svh, 42px);
    --profile-lane-gap: 8px;
  }
  .notice {
    margin-bottom: var(--space-2);
  }
</style>
