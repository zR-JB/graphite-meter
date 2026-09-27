<script lang="ts">
  import { catalogSelection } from "../presentation/serverAppearance";
  import { store } from "../state/store.svelte";
  import { reasonLabel, STAGE, STATUS } from "../presentation/vocabulary";
  import { LATENCY_LANES, type LatencyProfileViewLane } from "./latencyProfile";
  import LatencyProfileView from "./LatencyProfileView.svelte";
  import { handoff } from "../presentation/motion.svelte";

  const servers = $derived(
    store.serverDetails?.selection ??
      catalogSelection(store.serverCatalog, store.selectedServers),
  );
  const lanes = $derived<LatencyProfileViewLane[]>(
    LATENCY_LANES.filter(
      (meta) => store.stagePresentation[meta.key].configured,
    ).map((meta) => {
      const lane = store.latencyLanes.find((lane) => lane.key === meta.key)!;
      // A settled lane draws like a saved one: the latest reply only marks a running stage.
      return {
        ...lane,
        ...meta,
        current: lane.active ? lane.current : null,
      };
    }),
  );
  const profile = handoff(
    () => ({ run: store.runSeq, lanes }),
    (profile) => profile.run,
  );
</script>

<div class="live-profile" style:opacity={profile.opacity}>
  <LatencyProfileView
    lanes={profile.shown.lanes}
    variant="bare"
    added={store.result?.addedLatency}
    stability={store.result?.latency?.stabilityPct ?? null}
    source={servers.length > 1
      ? servers.find((server) => server.id === store.latencyFocus)?.name
      : undefined}
  />
  {#if store.stagePresentation.latency.status === "failed"}
    {@const failure = store.stagePresentation.latency.failure}
    <p class="notice" data-tone="err" role="alert">
      <strong>{STAGE.latency.label} {STATUS.failed.toLowerCase()}</strong>
      {failure ? reasonLabel(failure) : ""}
    </p>
  {/if}
</div>

<style>
  .live-profile {
    position: relative;
    display: grid;
    min-width: 0;
    min-height: 0;
    --profile-track-height: clamp(22px, 3.4svh, 34px);
    --profile-row: clamp(32px, 6.5svh, 64px);
  }
  .notice {
    position: absolute;
    inset: auto var(--space-4) var(--space-3);
  }
</style>
