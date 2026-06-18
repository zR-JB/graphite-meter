<script lang="ts">
  /* ============================================================
   * <WorkbenchDrawer> — power-user surface shell (§13.6)
   * A right-side slide-in drawer with a backdrop, focus trap,
   * Esc-to-close, and a tab switcher. The actual controls live in
   * three sub-panel components (modular, fixing linerate's
   * 1153-line monolith): Test Setup / Infrastructure / Developer.
   * All Test-Setup controls two-way bind to `console.config`.
   * ============================================================ */
  import { focusTrap } from "../../actions/focusTrap";
  import { ICON } from "../../constants";
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

  function close() {
    open = false;
  }
</script>

<div class="workbench-layer" class:open aria-hidden={!open}>
  <!-- Backdrop -->
  <button
    class="backdrop"
    aria-label="Close workbench"
    tabindex={open ? 0 : -1}
    onclick={close}
  ></button>

  <!-- Drawer -->
  {#if open}
    <div
      class="drawer"
      role="dialog"
      aria-modal="true"
      aria-label="Advanced workbench"
      tabindex="-1"
      use:focusTrap
      onkeydown={(e) => {
        if (e.key === "Escape") {
          e.stopPropagation();
          close();
        }
      }}
    >
      <header class="drawer-head">
        <div class="title">
          <span class="kicker">Advanced Layer</span>
          <h2>Workbench</h2>
        </div>
        <button
          class="close-btn"
          aria-label="Close workbench"
          title="Close (Esc)"
          onclick={close}
        >
          {@html ICON.close}
        </button>
      </header>

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

      <div class="drawer-body">
        {#if tab === "setup"}
          <TestSetupPanel running={store.isRunning} />
        {:else if tab === "infrastructure"}
          <InfrastructurePanel />
        {:else}
          <DeveloperPanel running={store.isRunning} />
        {/if}
      </div>
    </div>
  {/if}
</div>

<style>
  .workbench-layer {
    position: fixed;
    inset: 0;
    z-index: 60;
    pointer-events: none;
  }
  .workbench-layer:not(.open) {
    /* Inert when closed — the drawer is removed from the DOM via {#if}. */
    visibility: hidden;
  }

  .backdrop {
    position: fixed;
    inset: 0;
    border: 0;
    padding: 0;
    background: color-mix(in srgb, var(--canvas) 55%, transparent);
    opacity: 0;
    pointer-events: none;
    transition: opacity var(--dur-slide) var(--ease-out);
  }
  .workbench-layer.open .backdrop {
    opacity: 1;
    pointer-events: auto;
  }

  .drawer {
    position: fixed;
    top: 0;
    left: 0;
    bottom: 0;
    width: min(560px, 94vw);
    display: grid;
    grid-template-rows: auto auto minmax(0, 1fr);
    gap: 14px;
    border-right: 1px solid var(--border-strong);
    background: linear-gradient(180deg, var(--surface-2), var(--surface-1) 32%),
      var(--surface-1);
    box-shadow: var(--shadow-float);
    padding: 16px;
    pointer-events: auto;
    animation: drawer-in var(--dur-slide) var(--ease-out);
  }

  @keyframes drawer-in {
    from {
      transform: translateX(-100%);
    }
    to {
      transform: translateX(0);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .drawer {
      animation: none;
    }
    .backdrop {
      transition: none;
    }
  }

  .drawer-head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 14px;
  }
  .title .kicker {
    color: var(--brand-strong);
    font-family: var(--font-mono);
    font-size: 10px;
    font-weight: 800;
    letter-spacing: 0.14em;
    text-transform: uppercase;
  }
  .title h2 {
    margin: 2px 0 0;
    font-size: 22px;
    font-weight: 850;
    letter-spacing: -0.04em;
    color: var(--text);
  }

  .close-btn {
    display: grid;
    place-items: center;
    width: 34px;
    height: 34px;
    flex: none;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface-inset);
    color: var(--text-muted);
    transition:
      border-color var(--dur-hover) var(--ease-out),
      color var(--dur-hover) var(--ease-out),
      background var(--dur-hover) var(--ease-out);
  }
  .close-btn:hover {
    border-color: var(--border-strong);
    background: var(--surface-2);
    color: var(--text);
  }
  .close-btn :global(svg) {
    width: 18px;
    height: 18px;
  }

  /* Tab switcher — mirrors the .tool-group segmented pattern. */
  .tabs {
    display: flex;
    gap: 4px;
    padding: 4px;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--surface-inset);
  }
  .tab {
    flex: 1;
    min-height: 34px;
    border: 0;
    border-radius: var(--radius-md);
    background: transparent;
    color: var(--text-soft);
    font-size: 12px;
    font-weight: 800;
    cursor: pointer;
    transition:
      background var(--dur-hover) var(--ease-out),
      color var(--dur-hover) var(--ease-out);
  }
  .tab:hover {
    color: var(--text);
  }
  .tab.active {
    background: var(--brand-soft);
    color: var(--brand-strong);
  }

  .drawer-body {
    min-height: 0;
    overflow-y: auto;
    overflow-x: hidden;
    padding-right: 4px;
    scrollbar-gutter: stable;
  }
</style>
