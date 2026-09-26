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
  title="Details"
  kicker="Server & connection"
  label="Details"
  width="min(440px, 92vw)"
>
  <EndpointInfo />

  <p class="license">
    <span>Legal</span>
    <button
      class="btn-link"
      type="button"
      onclick={(event) =>
        onOpenLegal?.(event.currentTarget as unknown as HTMLElement)}
      >About &amp; legal</button
    >
  </p>
</SidePanel>

<style>
  .license {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: var(--space-2);
    padding: 0 var(--space-1);
    color: var(--text-soft);
    font-size: var(--type-xs);
  }
</style>
