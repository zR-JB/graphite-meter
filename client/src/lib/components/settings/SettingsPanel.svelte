<script lang="ts">
  /* ============================================================
   * <SettingsPanel> — advanced settings surface (§13.6)
   * A thin wrapper over the shared <SidePanel> (left side) so it
   * looks and behaves exactly like the Connection & telemetry panel.
   * Owns only the tab switcher + which sub-panel is shown; the shell,
   * header, backdrop, slide, focus trap and Esc all live in SidePanel.
   * The controls live in three tabs: Test Setup / Infrastructure /
   * Developer. All Test-Setup controls two-way bind to store.config.
   * ============================================================ */
  import SidePanel from "../SidePanel.svelte";
  import { store } from "../../state/store.svelte";
  import TestSetupPanel from "./TestSetupPanel.svelte";
  import InfrastructurePanel from "./InfrastructurePanel.svelte";
  import DeveloperPanel from "./DeveloperPanel.svelte";
  import { tooltip } from "../../actions/tooltip";

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
  type Tab = "setup" | "infrastructure" | "developer";

  // The Developer tab only exists when the build includes dev tools
  // (GM_CLIENT_DEV_TOOLS). The `...(false ? [...] : [])` spread folds to nothing
  // in a prod build, and the gated render branch below drops the import — so the
  // whole DeveloperPanel (debug logging + simulation) leaves the bundle.
  const TABS: { key: Tab; label: string }[] = [
    { key: "setup", label: "Test Setup" },
    { key: "infrastructure", label: "Infrastructure" },
    ...(__GM_DEV_TOOLS__ ? [{ key: "developer" as Tab, label: "Developer" }] : []),
  ];
</script>

<SidePanel
  bind:open
  {docked}
  {raised}
  {dockWidth}
  {onResize}
  {onResetWidth}
  side="left"
  title="Settings"
  kicker="Advanced Layer"
  label="Settings"
  width="min(560px, 94vw)"
>
  {#snippet toolbar()}
    <div class="display-controls" aria-label="Display units">
      <!-- Display-only; persisted. Drives every rate label, gauge/chart tick,
           and result card without changing the measured raw bytes/sec. -->
      <div class="unit-seg" role="group" aria-label="Rate unit">
        <button
          type="button"
          class:active={store.unitKind === "bits"}
          aria-pressed={store.unitKind === "bits"}
          use:tooltip={"Show rates in bits per second (Mbit/s, Gbit/s)"}
          onclick={() => (store.unitKind = "bits")}>Bit</button>
        <button
          type="button"
          class:active={store.unitKind === "bytes"}
          aria-pressed={store.unitKind === "bytes"}
          use:tooltip={"Show rates in bytes per second (MB/s, GB/s)"}
          onclick={() => (store.unitKind = "bytes")}>Byte</button>
      </div>
      <div class="unit-seg" role="group" aria-label="Unit base">
        <button
          type="button"
          class:active={store.unitBase === "base10"}
          aria-pressed={store.unitBase === "base10"}
          use:tooltip={"Base-10 / SI prefixes (1 k = 1000) — Mbit/s, Gbit/s"}
          onclick={() => (store.unitBase = "base10")}>BASE10</button>
        <button
          type="button"
          class:active={store.unitBase === "base2"}
          aria-pressed={store.unitBase === "base2"}
          use:tooltip={"Base-2 / IEC prefixes (1 Ki = 1024) — Mibit/s, Gibit/s"}
          onclick={() => (store.unitBase = "base2")}>BASE2</button>
      </div>
    </div>

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

  {#if store.settingsTab === "infrastructure"}
    <InfrastructurePanel />
  {:else if __GM_DEV_TOOLS__ && store.settingsTab === "developer"}
    <DeveloperPanel running={store.isRunning} />
  {:else}
    <!-- Default + fallback (also when a persisted "developer" tab was stripped). -->
    <TestSetupPanel running={store.isRunning} />
  {/if}
</SidePanel>

<style>
  .display-controls {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    gap: var(--space-2);
    margin-bottom: var(--space-3);
    min-width: 0;
  }

  .unit-seg {
    display: flex;
    gap: 2px;
    min-width: 0;
    height: 36px;
    padding: 2px;
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-inset);
    box-shadow: var(--elev-recess);
  }
  .unit-seg button {
    flex: 1;
    min-width: 0;
    padding: 0 var(--space-2);
    border: 0;
    border-radius: calc(var(--r-chrome) - 2px);
    background: transparent;
    color: var(--text-muted);
    font-family: var(--font-mono);
    font-size: 11px;
    font-weight: 800;
    cursor: pointer;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    transition:
      background var(--dur-hover) var(--ease-out),
      color var(--dur-hover) var(--ease-out);
  }
  .unit-seg button:hover {
    color: var(--text);
  }
  .unit-seg button.active {
    background: var(--brand-soft);
    box-shadow: var(--elev-tile);
    color: var(--brand-strong);
  }

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

  @media (max-width: 420px) {
    .display-controls {
      grid-template-columns: 1fr;
    }
  }
</style>
