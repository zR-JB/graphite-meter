<script lang="ts">
  // Endpoint info wrapper around the shared docked/flyout panel.
  import SidePanel from "./SidePanel.svelte";
  import EndpointInfo from "./EndpointInfo.svelte";

  interface Props {
    open?: boolean;
    docked?: boolean;
    raised?: boolean;
    dockWidth?: number;
    dockMaxWidth?: number;
    onResize?: (px: number) => void;
    onResetWidth?: () => void;
    onOpenLegal?: (invoker: HTMLElement) => void;
    onClose?: () => void;
  }
  let {
    open = $bindable(false),
    docked = false,
    raised = false,
    dockWidth,
    dockMaxWidth,
    onResize,
    onResetWidth,
    onOpenLegal,
    onClose,
  }: Props = $props();
</script>

<SidePanel
  bind:open
  {docked}
  {raised}
  {dockWidth}
  {dockMaxWidth}
  {onResize}
  {onResetWidth}
  {onClose}
  side="right"
  title="Endpoint"
  kicker="Info"
  label="Endpoint info"
  width="min(440px, 92vw)"
>
  <EndpointInfo />

  <p class="license">
    <span>Legal</span>
    <button
      type="button"
      onclick={(event) =>
        onOpenLegal?.(event.currentTarget as unknown as HTMLElement)}
      >About &amp; legal</button
    >
  </p>
</SidePanel>

<style>
  /* Footer legal line keeps the previous Endpoint Info visual weight. */
  .license {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: var(--space-2);
    padding: 0 var(--space-1);
    font-size: var(--type-xs);
    color: var(--text-soft);
  }
  .license button {
    border: 0;
    padding: 0;
    background: transparent;
    color: var(--text-muted);
    font-family: var(--font-mono);
    text-decoration: underline;
    transition: color var(--dur-hover) var(--ease-out);
    cursor: pointer;
  }
  .license button:hover {
    color: var(--text);
  }
  .license button:focus-visible {
    outline: var(--focus-ring);
    outline-offset: 2px;
    border-radius: var(--r-well);
  }
</style>
