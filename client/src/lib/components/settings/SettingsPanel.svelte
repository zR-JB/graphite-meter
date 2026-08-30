<script lang="ts">
  // Settings drawer wrapper. The shared SidePanel owns docking, focus,
  // dismissal, and mobile sheet behavior.
  import SidePanel from "../SidePanel.svelte";
  import { store } from "../../state/store.svelte";
  import TestSetupPanel from "./TestSetupPanel.svelte";
  import ConfirmDialog from "../ConfirmDialog.svelte";

  interface Props {
    open?: boolean;
    docked?: boolean;
    raised?: boolean;
    dockWidth?: number;
    onResize?: (px: number) => void;
    onResetWidth?: () => void;
    onClose?: () => void;
    onOpenHistory?: (invoker: HTMLElement) => void;
  }
  let {
    open = $bindable(false),
    docked = false,
    raised = false,
    dockWidth,
    onResize,
    onResetWidth,
    onClose,
    onOpenHistory,
  }: Props = $props();

  let resetConfirmOpen = $state(false);
  let setupResetVersion = $state(0);

  function confirmSettingsReset() {
    resetConfirmOpen = false;
    store.restoreTestDisplayDefaults();
    setupResetVersion++;
  }
</script>

<SidePanel
  bind:open
  {docked}
  {raised}
  {dockWidth}
  {onResize}
  {onResetWidth}
  {onClose}
  side="left"
  title="Settings"
  kicker="Test & Display"
  label="Settings"
  width="min(560px, 94vw)"
>
  {#key setupResetVersion}
    <TestSetupPanel running={store.isRunning} {onOpenHistory} />
  {/key}
  <div class="settings-reset">
    <button
      type="button"
      disabled={store.isRunning}
      onclick={() => (resetConfirmOpen = true)}>Reset settings</button
    >
  </div>
</SidePanel>

<ConfirmDialog
  open={resetConfirmOpen}
  id="settings-reset-confirm"
  title="Reset settings?"
  description="Restore test and display settings to the shipped defaults? Your theme, panel layout, and existing test results will be kept."
  cancelLabel="Keep settings"
  confirmLabel="Reset settings"
  onCancel={() => (resetConfirmOpen = false)}
  onConfirm={confirmSettingsReset}
/>

<style>
  .settings-reset {
    display: grid;
    justify-items: start;
    padding-top: var(--space-3);
    border-top: 1px solid var(--border);
  }
  .settings-reset button {
    min-height: 32px;
    padding: 5px 10px;
    border: 1px solid color-mix(in srgb, var(--err) 42%, var(--border));
    border-radius: var(--r-chrome);
    background: transparent;
    color: var(--text-soft);
    font-size: var(--type-sm);
    font-weight: 700;
    cursor: pointer;
  }
  .settings-reset button:hover:not(:disabled) {
    border-color: var(--err);
    color: var(--err);
  }
  .settings-reset button:disabled {
    cursor: not-allowed;
    opacity: 0.5;
  }
</style>
