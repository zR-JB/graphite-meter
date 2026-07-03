<script lang="ts">
  /* ============================================================
   * <SettingsPanel> — advanced settings surface
   * A thin wrapper over the shared <SidePanel> (left side) so it
   * looks and behaves exactly like the Connection & telemetry panel.
   * Owns only the tab switcher + which sub-panel is shown; the shell,
   * header, backdrop, slide, focus trap and Esc all live in SidePanel.
   * Two tabs: Test Setup / Developer (dev builds only — a prod build
   * is Setup-only, so the tab bar itself disappears). All Test-Setup
   * controls two-way bind to store.config.
   * ============================================================ */
  import SidePanel from "../SidePanel.svelte";
  import { store } from "../../state/store.svelte";
  import TestSetupPanel from "./TestSetupPanel.svelte";
  import DeveloperPanel from "./DeveloperPanel.svelte";

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

  // The active tab lives in the store (persisted), so reopening Settings lands
  // on the last-viewed section rather than resetting to Test Setup.
  type Tab = "setup" | "developer";

  // The Developer tab only exists when the build includes dev tools
  // (GM_CLIENT_DEV_TOOLS). The `...(false ? [...] : [])` spread folds to nothing
  // in a prod build, and the gated render branch below drops the import — so the
  // whole DeveloperPanel (debug logging + simulation) leaves the bundle. With
  // Setup then the only tab, the tab bar itself is not rendered.
  const TABS: { key: Tab; label: string }[] = [
    { key: "setup", label: "Setup" },
    ...(__GM_DEV_TOOLS__ ? [{ key: "developer" as Tab, label: "Developer" }] : []),
  ];
</script>

{#snippet tabs()}
  <div class="tabs" role="tablist" aria-label="Settings sections">
    {#each TABS as t (t.key)}
      <button
        class="tab"
        role="tab"
        class:active={store.settingsTab === t.key}
        aria-selected={store.settingsTab === t.key}
        onclick={() => (store.settingsTab = t.key)}
      >
        {t.label}
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
  kicker="Setup & Tuning"
  label="Settings"
  width="min(560px, 94vw)"
  toolbar={TABS.length > 1 ? tabs : undefined}
>
  {#if __GM_DEV_TOOLS__ && store.settingsTab === "developer"}
    <DeveloperPanel running={store.isRunning} />
  {:else}
    <!-- Default + fallback (also when a persisted "developer" tab was stripped). -->
    <TestSetupPanel running={store.isRunning} />
  {/if}
</SidePanel>

<style>
  /* Tab switcher — a recessed segmented track with the active tab
     lifted as a milled tile. */
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
  .tab.active {
    background: var(--brand-soft);
    box-shadow: var(--elev-tile);
    color: var(--brand-strong);
  }

</style>
