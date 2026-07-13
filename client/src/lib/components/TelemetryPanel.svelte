<script lang="ts">
  // Endpoint info wrapper around the shared docked/flyout panel.
  import SidePanel from "./SidePanel.svelte";
  import EndpointInfo from "./EndpointInfo.svelte";

  interface Props {
    open?: boolean;
    docked?: boolean;
    raised?: boolean;
    dockWidth?: number;
    onResize?: (px: number) => void;
    onResetWidth?: () => void;
  }
  let {
    open = $bindable(false),
    docked = false,
    raised = false,
    dockWidth,
    onResize,
    onResetWidth,
  }: Props = $props();
</script>

<SidePanel
  bind:open
  {docked}
  {raised}
  {dockWidth}
  {onResize}
  {onResetWidth}
  side="right"
  title="Endpoint"
  kicker="Info"
  label="Endpoint info"
  width="min(440px, 92vw)"
>
  <EndpointInfo />

  <p class="license">
    <span>License</span>
    <a
      href="https://www.gnu.org/licenses/agpl-3.0.html"
      target="_blank"
      rel="license noopener noreferrer">AGPL-3.0-or-later</a
    >
  </p>
</SidePanel>

<style>
  /* Footer license line — quiet, label + linked SPDX id. */
  .license {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: var(--space-2);
    padding: 0 var(--space-1);
    font-size: var(--type-xs);
    color: var(--text-soft);
  }
  .license a {
    color: var(--text-muted);
    font-family: var(--font-mono);
    text-decoration: none;
    transition: color var(--dur-hover) var(--ease-out);
  }
  .license a:hover {
    color: var(--text);
    text-decoration: underline;
  }
  .license a:focus-visible {
    outline: 2px solid color-mix(in srgb, var(--brand) 70%, transparent);
    outline-offset: 2px;
    border-radius: 2px;
  }
</style>
