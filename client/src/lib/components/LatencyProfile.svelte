<script lang="ts">
  import {
    catalogSelection,
    serverName,
  } from "../presentation/serverAppearance";
  import { store } from "../state/store.svelte";
  import { reasonLabel, STAGE, STATUS } from "../presentation/vocabulary";
  import { LATENCY_LANES, type LatencyProfileViewLane } from "./latencyProfile";
  import LatencyProfileView from "./LatencyProfileView.svelte";
  import { announceChanges } from "../presentation/announcer.svelte";

  const servers = $derived(
    store.serverDetails?.selection ??
      catalogSelection(store.serverCatalog, store.selectedServers),
  );
  // Lost probes name their reason on their population's row, by server when several ran, so the card never grows.
  const notes = $derived.by(() => {
    const details = store.result?.multiServer ?? store.serverDetails;
    const notes: Partial<Record<LatencyProfileViewLane["key"], string[]>> = {};
    if (!details) return notes;
    const several = details.selection.length > 1;
    for (const failure of details.failures) {
      if (failure.scope !== "latency") continue;
      if (store.resultScope && failure.serverId !== store.resultScope) continue;
      const reason = reasonLabel(failure.reason);
      (notes[failure.stage] ??= []).push(
        several
          ? `${serverName(details.selection, failure.serverId)}: ${reason.charAt(0).toLowerCase()}${reason.slice(1)}`
          : reason,
      );
    }
    return notes;
  });
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
        failure: notes[meta.key]?.join("\n"),
      };
    }),
  );
  const saved = $derived(store.result && store.latencyServer);
  const stage = $derived(store.stagePresentation.latency);
  const failure = $derived(
    stage.status !== "failed"
      ? undefined
      : stage.failure
        ? reasonLabel(stage.failure)
        : STATUS.failed,
  );
  // The reason stands in the caption's place; a screen reader hears it once, when the stage fails.
  announceChanges(() =>
    stage.status !== "failed"
      ? ""
      : [
          `${STAGE.latency.label} ${STATUS.failed.toLowerCase()}`,
          stage.failure && reasonLabel(stage.failure),
        ]
          .filter(Boolean)
          .join(": "),
  );
</script>

<div class="live-profile">
  <LatencyProfileView
    {lanes}
    variant="bare"
    added={saved?.addedLatency}
    {failure}
    source={servers.length > 1
      ? servers.find((server) => server.id === store.latencyFocus)?.name
      : undefined}
  />
</div>

<style>
  .live-profile {
    display: grid;
    flex: 1 1 auto;
    min-width: 0;
    min-height: 0;
  }
</style>
