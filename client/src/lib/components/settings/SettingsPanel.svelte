<script lang="ts">
  // Settings drawer wrapper and optional developer tab. The shared SidePanel
  // owns docking, focus, dismissal, and mobile sheet behavior.
  import SidePanel from "../SidePanel.svelte";
  import { store } from "../../state/store.svelte";
  import TestSetupPanel from "./TestSetupPanel.svelte";
  import DeveloperPanel from "./DeveloperPanel.svelte";
  import ConfirmDialog from "../ConfirmDialog.svelte";

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

  let resetConfirmOpen = $state(false);
  let setupResetVersion = $state(0);

  function confirmSettingsReset() {
    resetConfirmOpen = false;
    store.restoreTestDisplayDefaults();
    setupResetVersion++;
  }

  // The active tab is store.settingsTab, persisted across reopens.
  type Tab = "setup" | "developer";

  // __GM_DEV_TOOLS__ is a build-time constant. In a prod build this list and
  // the gated branch below both fold away, dropping DeveloperPanel and its
  // imports from the bundle.
  const DEVELOPER_TAB: { key: Tab; label: string }[] = __GM_DEV_TOOLS__
    ? [{ key: "developer", label: "Developer" }]
    : [];
  const TABS: { key: Tab; label: string }[] = [
    { key: "setup", label: "Setup" },
    ...DEVELOPER_TAB,
  ];
</script>

{#snippet tabs()}
  <div class="tabs" role="tablist" aria-label="Settings sections">
    {#each TABS as tab (tab.key)}
      <button
        class="tab"
        role="tab"
        class:active={store.settingsTab === tab.key}
        aria-selected={store.settingsTab === tab.key}
        onclick={() => (store.settingsTab = tab.key)}
      >
        {tab.label}
      </button>
    {/each}
  </div>
{/snippet}

<SidePanel
  bind:open
  {docked}
  {raised}
  {dockWidth}
  {onResize}
  {onResetWidth}
  side="left"
  title="Settings"
  kicker="Test & Display"
  label="Settings"
  width="min(560px, 94vw)"
  toolbar={TABS.length > 1 ? tabs : undefined}
>
  {#if __GM_DEV_TOOLS__ && store.settingsTab === "developer"}
    <DeveloperPanel running={store.isRunning} />
  {:else}
    <!-- Default, and the fallback for a persisted "developer" tab in a prod build. -->
    {#key setupResetVersion}
      <TestSetupPanel running={store.isRunning} />
    {/key}
    <div class="settings-reset">
      <button
        type="button"
        disabled={store.isRunning}
        onclick={() => (resetConfirmOpen = true)}>Reset settings</button
      >
    </div>
  {/if}
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
  /* Recessed segmented track; the active tab lifts as a milled tile. */
  .tabs {
    display: flex;
    gap: var(--space-1);
    padding: var(--space-1);
    min-width: 0;
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-inset);
    box-shadow: var(--elev-recess);
  }
  .tab {
    flex: 1;
    min-width: 0;
    min-height: 34px;
    border: 0;
    border-radius: var(--r-well);
    background: transparent;
    color: var(--text-soft);
    font-size: var(--type-sm);
    font-weight: 800;
    cursor: pointer;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    transition:
      background var(--dur-hover) var(--ease-out),
      color var(--dur-hover) var(--ease-out);
  }
  .tab:hover {
    color: var(--text);
  }
  /* Inside the recessed track: the ring rides the tab's own edge. */
  .tab:focus-visible {
    outline: var(--focus-ring);
    outline-offset: -2px;
  }
  .tab.active {
    background: var(--brand-soft);
    box-shadow: var(--elev-tile);
    color: var(--brand-strong);
  }
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
