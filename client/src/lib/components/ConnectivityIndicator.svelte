<script lang="ts">
  import { store } from "../state/store.svelte";
  import { tooltip } from "../actions/tooltip";
  import { announceChanges } from "../presentation/announcer.svelte";
  import {
    CONNECTIVITY,
    counted,
    reasonLabel,
  } from "../presentation/vocabulary";
  import { recentProbes } from "../state/connectionHealth";
  import { median } from "../runner/measure";
  import { fmtMs } from "../format";

  const spark = $derived(
    store.pulseLatency
      .slice(-16)
      .flatMap((bucket) =>
        bucket.medianRttMs == null ? [] : [bucket.medianRttMs],
      ),
  );

  const points = $derived.by(() => {
    if (spark.length < 2) return "";
    const min = Math.min(...spark);
    const range = Math.max(...spark) - min || 1;
    return spark
      .map(
        (rttMs, i) =>
          `${1 + (i / (spark.length - 1)) * 34},${15 - ((rttMs - min) / range) * 14}`,
      )
      .join(" ");
  });
  const state = $derived(CONNECTIVITY[store.effectiveConnectivity]);
  const label = $derived(`Connection: ${state.label}`);
  // The values behind the state, so the dot is never a verdict alone.
  const facts = $derived.by(() => {
    const { replies, probes, timeouts } = recentProbes(store.pulseLatency);
    const selected = store.selectedServers;
    return [
      store.effectiveConnectivity === "recovering" && store.stallInfo
        ? reasonLabel(store.stallInfo.reason)
        : "",
      !store.isRunning && store.selectionValidation === "failed"
        ? `${selected.filter((id) => store.servers.get(id)?.readiness === "verified").length} of ${counted(selected.length, "server")} ready`
        : "",
      replies.length
        ? `Median ${fmtMs(median(replies))} ms, last 4 s of probes`
        : "",
      probes
        ? `${timeouts} of ${counted(probes, "probe")} timed out`
        : "No probes yet",
    ].filter(Boolean);
  });
  // Only settled trouble is spoken; checking flaps every run and the toast speaks a stall.
  announceChanges(() =>
    ["degraded", "unstable", "offline"].includes(store.effectiveConnectivity)
      ? label
      : "",
  );
</script>

<div
  class="pulse"
  tabindex="-1"
  {@attach tooltip(() => [label, ...facts].join("\n"))}
>
  <span class="sr-only">{[label, ...facts].join(". ")}</span>
  <span class="status-dot" data-tone={state.tone}></span>
  <svg class="spark" viewBox="0 0 36 16" aria-hidden="true">
    <polyline {points} />
  </svg>
</div>

<style>
  .pulse {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    padding: 0 6px;
  }
  .spark {
    width: 36px;
    height: 16px;
    display: block;
    opacity: 0.85;
  }

  polyline {
    fill: none;
    stroke: var(--text-soft);
    stroke-width: 1;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
</style>
