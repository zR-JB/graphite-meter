<script lang="ts">
  // Live wrapper around the shared interactive latency distribution. Saved
  // results use the same LatencyProfileView implementation with finalized data.
  import type { TransportRole } from "../runner/contract";
  import { store } from "../state/store.svelte";
  import { failureDetail } from "./failurePresentation";
  import type { LatencyProfileViewLane } from "./latencyProfile";
  import LatencyProfileView from "./LatencyProfileView.svelte";

  const META: Record<
    TransportRole,
    Pick<LatencyProfileViewLane, "label" | "tone">
  > = {
    latency: { label: "Idle", tone: "latency" },
    download: { label: "Loaded Down", tone: "download" },
    upload: { label: "Loaded Up", tone: "upload" },
    bidirectional: { label: "Loaded Bi-dir", tone: "bidirectional" },
  };

  const lanes = $derived<LatencyProfileViewLane[]>(
    store.latencyLanes
      .filter((lane) => store.stagePresentation[lane.key].configured)
      .map((lane) => ({
        ...lane,
        ...META[lane.key],
      })),
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

  <LatencyProfileView {lanes} variant="bare" showCurrent showTimeouts />
</section>

<style>
  .live-profile {
    height: 100%;
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
