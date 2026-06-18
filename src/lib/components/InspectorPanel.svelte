<script lang="ts">
  /* ============================================================
   * <InspectorPanel> — Connection & telemetry (§3.7)
   * Mirror of <WorkbenchDrawer>: a thin wrapper over the shared
   * <SidePanel> (right side), so the two auxiliary panels are visually
   * and behaviourally identical. Shows the connection InfraCard, with
   * the heavier percentiles/jitter detail tucked behind an opt-in
   * disclosure so the panel stays light by default.
   * ============================================================ */
  import SidePanel from "./SidePanel.svelte";
  import InfraCard from "./InfraCard.svelte";
  import TelemetryDetail from "./TelemetryDetail.svelte";

  interface Props {
    open?: boolean;
    docked?: boolean;
    dockWidth?: number;
    onResize?: (px: number) => void;
    onResetWidth?: () => void;
  }
  let {
    open = $bindable(false),
    docked = false,
    dockWidth,
    onResize,
    onResetWidth,
  }: Props = $props();
</script>

<SidePanel
  bind:open
  {docked}
  {dockWidth}
  {onResize}
  {onResetWidth}
  side="right"
  title="Connection & telemetry"
  kicker="Live"
  label="Connection and telemetry info"
  width="min(440px, 92vw)"
>
  <InfraCard />
  <details class="telemetry-disclose">
    <summary>Show detailed telemetry</summary>
    <div class="telemetry-disclose__body">
      <TelemetryDetail />
    </div>
  </details>
</SidePanel>

<style>
  /* Opt-in disclosure wrapping the heavy telemetry (§14.2). */
  .telemetry-disclose {
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-1);
    box-shadow: var(--elev-tile);
    overflow: clip;
  }
  .telemetry-disclose > summary {
    list-style: none;
    cursor: pointer;
    padding: 13px 14px;
    font-size: var(--type-sm);
    font-weight: 700;
    letter-spacing: 0.02em;
    color: var(--text-muted);
    transition: color var(--dur-hover) var(--ease-out);
  }
  .telemetry-disclose > summary::-webkit-details-marker {
    display: none;
  }
  .telemetry-disclose > summary::before {
    content: "▸";
    display: inline-block;
    margin-right: 8px;
    color: var(--text-soft);
    transition: transform var(--dur-hover) var(--ease-out);
  }
  .telemetry-disclose[open] > summary::before {
    transform: rotate(90deg);
  }
  .telemetry-disclose > summary:hover {
    color: var(--text);
  }
  .telemetry-disclose > summary:focus-visible {
    outline: 2px solid color-mix(in srgb, var(--brand) 70%, transparent);
    outline-offset: -2px;
  }
  /* The nested card already has its own border/radius; drop the top border so
     it reads as one continuous panel under the summary. */
  .telemetry-disclose__body :global(.card) {
    border: 0;
    border-top: 1px solid var(--border);
    border-radius: 0;
    box-shadow: none;
  }
</style>
