<script lang="ts">
  import {
    pos,
    rangeWidth,
    type LatencyProfileDomain,
    type LatencyProfileLaneLike,
  } from "./latencyProfile";

  interface Props {
    lane: LatencyProfileLaneLike & { lossRatio?: number };
    domain: LatencyProfileDomain;
    showCurrent?: boolean;
    showLoss?: boolean;
    tone?: "latency" | "download" | "upload" | "bidirectional";
    compact?: boolean;
  }
  let {
    lane,
    domain,
    showCurrent = false,
    showLoss = false,
    tone = "latency",
    compact = false,
  }: Props = $props();
  const toneVars = {
    latency: "var(--phase-latency)",
    download: "var(--phase-download)",
    upload: "var(--phase-upload)",
    bidirectional: "var(--phase-bidirectional)",
  } as const;
</script>

<span
  class="profile-marks"
  class:compact
  style={`--profile-tone:${toneVars[tone]}`}
>
  {#if lane.min != null && lane.max != null}
    <span
      class="range"
      style={`left:${pos(lane.min, domain)}%;width:${rangeWidth(lane.min, lane.max, domain)}%`}
    ></span>
  {/if}
  {#if lane.p10 != null && lane.p90 != null}
    <span
      class="band"
      style={`left:${pos(lane.p10, domain)}%;width:${rangeWidth(lane.p10, lane.p90, domain)}%`}
    ></span>
  {/if}
  {#if lane.center != null}
    <i class="center-marker" style={`left:${pos(lane.center, domain)}%`}></i>
  {/if}
  {#if showCurrent && lane.current != null}
    <i class="cur-marker" style={`left:${pos(lane.current, domain)}%`}></i>
  {/if}
  {#if showLoss && (lane.lossRatio ?? 0) > 0}
    <i
      class="loss-marker"
      style={`width:${Math.min(34, Math.max(8, (lane.lossRatio ?? 0) * 100))}%`}
    ></i>
  {/if}
</span>

<style>
  .range,
  .band,
  .center-marker,
  .cur-marker,
  .loss-marker {
    position: absolute;
  }
  .range {
    top: 13px;
    height: 5px;
    min-width: 10px;
    border-radius: var(--r-full);
    background: color-mix(in srgb, var(--text-soft) 40%, transparent);
  }
  .range::before,
  .range::after {
    position: absolute;
    top: -7px;
    width: 1px;
    height: 19px;
    content: "";
    background: color-mix(in srgb, var(--text-soft) 64%, transparent);
  }
  .range::after {
    right: 0;
  }
  .range::before {
    left: 0;
  }
  .band {
    top: 6px;
    height: 18px;
    min-width: 8px;
    border-radius: var(--r-full);
    background: color-mix(
      in srgb,
      var(--profile-tone, var(--signal)) 28%,
      transparent
    );
    box-shadow: inset 0 0 0 1px
      color-mix(in srgb, var(--profile-tone, var(--signal)) 30%, transparent);
  }
  .center-marker {
    top: 5px;
    bottom: 5px;
    width: 2px;
    border-radius: var(--r-full);
    background: color-mix(in srgb, var(--text) 54%, transparent);
    transform: translateX(-50%);
  }
  .cur-marker {
    top: 9px;
    bottom: 9px;
    width: 10px;
    border: 2px solid var(--surface-1);
    border-radius: var(--r-full);
    background: var(--profile-tone, var(--signal-strong));
    box-shadow: 0 0 0 2px
      color-mix(in srgb, var(--profile-tone, var(--signal)) 18%, transparent);
    transform: translateX(-50%);
  }
  .loss-marker {
    top: 0;
    right: 0;
    bottom: 0;
    min-width: 8px;
    border-radius: var(--r-full);
    background: repeating-linear-gradient(
      -45deg,
      var(--err) 0 4px,
      color-mix(in srgb, var(--err) 44%, transparent) 4px 8px
    );
    opacity: 0.82;
  }
  .profile-marks.compact .range {
    top: 7px;
    height: 3px;
    min-width: 8px;
  }
  .profile-marks.compact .range::before,
  .profile-marks.compact .range::after {
    top: -4px;
    height: 11px;
  }
  .profile-marks.compact .band {
    top: 3px;
    height: 11px;
    min-width: 6px;
  }
  .profile-marks.compact .center-marker {
    top: 2px;
    bottom: 2px;
  }
  .profile-marks.compact .cur-marker {
    top: 4px;
    bottom: 4px;
    width: 8px;
  }
</style>
