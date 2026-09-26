<script lang="ts">
  import { catalogSelection } from "../presentation/serverAppearance";
  import { store } from "../state/store.svelte";
  import { getApplicationController } from "../runner/controllerContext";
  import ServerScope from "./ServerScope.svelte";
  import { failureDetail } from "./failurePresentation";
  import { LATENCY_LANES, type LatencyProfileViewLane } from "./latencyProfile";
  import LatencyProfileView from "./LatencyProfileView.svelte";

  const controller = getApplicationController();
  const servers = $derived(
    store.serverDetails?.selection ??
      catalogSelection(store.serverCatalog, store.selectedServers),
  );
  const unmeasured = $derived(
    servers
      .filter((server) =>
        store.serverDetails
          ? !store.serverDetails.servers.some(
              (result) =>
                result.server.id === server.id && result.latencyTarget,
            )
          : store.latencySelection.mode === "primary" &&
            server.id !== store.primaryLatencyServer,
      )
      .map((server) => server.id),
  );
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
  {#if servers.length > 1}
    <div class="latency-focus">
      <ServerScope
        {servers}
        value={store.latencyFocus}
        label="Latency server shown in gauge, profile and chart"
        disabled={servers.length - unmeasured.length < 2}
        disabledIds={unmeasured}
        onchange={controller.focusServer}
      />
    </div>
  {/if}
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
  .latency-focus {
    display: flex;
    justify-content: flex-end;
    margin-bottom: var(--space-2);
  }
</style>
