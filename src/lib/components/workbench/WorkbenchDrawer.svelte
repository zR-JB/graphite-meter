<script lang="ts">
  /* ============================================================
   * <WorkbenchDrawer> — power-user surface (§13.6)
   * Now a thin wrapper over the shared <SidePanel> (left side) so it
   * looks and behaves exactly like the Connection & telemetry panel.
   * Owns only the tab switcher + which sub-panel is shown; the shell,
   * header, backdrop, slide, focus trap and Esc all live in SidePanel.
   * The controls live in three sub-panels: Test Setup / Infrastructure
   * / Developer. All Test-Setup controls two-way bind to console.config.
   * ============================================================ */
  import SidePanel from "../SidePanel.svelte";
  import { console as store } from "../../state/console.svelte";
  import TestSetupPanel from "./TestSetupPanel.svelte";
  import InfrastructurePanel from "./InfrastructurePanel.svelte";
  import DeveloperPanel from "./DeveloperPanel.svelte";

  interface Props {
    open?: boolean;
  }
  let { open = $bindable(false) }: Props = $props();

  type Tab = "setup" | "infrastructure" | "developer";
  let tab = $state<Tab>("setup");

  const TABS: { key: Tab; label: string }[] = [
    { key: "setup", label: "Test Setup" },
    { key: "infrastructure", label: "Infrastructure" },
    { key: "developer", label: "Developer" },
  ];
</script>

<SidePanel
  bind:open
  side="left"
  title="Workbench"
  kicker="Advanced Layer"
  label="Advanced workbench"
  width="min(560px, 94vw)"
>
  {#snippet toolbar()}
    <div class="tabs" role="tablist" aria-label="Workbench sections">
      {#each TABS as t (t.key)}
        <button
          class="tab"
          role="tab"
          class:active={tab === t.key}
          aria-selected={tab === t.key}
          onclick={() => (tab = t.key)}
        >
          {t.label}
        </button>
      {/each}
    </div>
  {/snippet}

  {#if tab === "setup"}
    <TestSetupPanel running={store.isRunning} />
  {:else if tab === "infrastructure"}
    <InfrastructurePanel />
  {:else}
    <DeveloperPanel running={store.isRunning} />
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
