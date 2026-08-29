<script lang="ts">
  import { onMount, tick } from "svelte";
  import { ICON } from "../constants";
  import type { ThemePref } from "../state/persistence";

  interface Props {
    showHistory: boolean;
    showEndpoint: boolean;
    showTheme: boolean;
    historyActive: boolean;
    endpointActive: boolean;
    theme: ThemePref;
    onHistory: () => void;
    onEndpoint: () => void;
    onTheme: () => void;
  }

  let {
    showHistory,
    showEndpoint,
    showTheme,
    historyActive,
    endpointActive,
    theme,
    onHistory,
    onEndpoint,
    onTheme,
  }: Props = $props();
  let open = $state(false);
  let trigger = $state<HTMLButtonElement>();
  let menu = $state<HTMLDivElement>();

  async function toggle() {
    open = !open;
    if (open) {
      await tick();
      menu?.querySelector<HTMLButtonElement>("button")?.focus();
    }
  }

  function choose(action: () => void) {
    open = false;
    action();
  }

  onMount(() => {
    const outside = (event: PointerEvent) => {
      const target = event.target;
      if (!(target instanceof Node)) return;
      if (!trigger?.contains(target) && !menu?.contains(target)) open = false;
    };
    document.addEventListener("pointerdown", outside);
    return () => document.removeEventListener("pointerdown", outside);
  });
</script>

<div class="more-control">
  <button
    bind:this={trigger}
    class:active={showHistory && historyActive}
    class="more-trigger"
    type="button"
    aria-label="More controls"
    aria-haspopup="menu"
    aria-expanded={open}
    onclick={toggle}
  >
    {@html ICON.more}
  </button>
  {#if open}
    <div
      bind:this={menu}
      class="more-menu"
      role="menu"
      tabindex="-1"
      aria-label="More controls"
      onkeydown={(event) => {
        if (event.key !== "Escape") return;
        event.preventDefault();
        open = false;
        trigger?.focus();
      }}
    >
      {#if showHistory}
        <button
          type="button"
          role="menuitem"
          aria-current={historyActive ? "page" : undefined}
          onclick={() => choose(onHistory)}
        >
          <span class="history-icon">{@html ICON.history}</span>
          <span
            ><strong>{historyActive ? "Close History" : "Open History"}</strong
            ><small>Local saved results</small></span
          >
        </button>
      {/if}
      {#if showEndpoint}
        <button
          type="button"
          role="menuitem"
          aria-current={endpointActive ? "true" : undefined}
          onclick={() => choose(onEndpoint)}
        >
          <span>{@html ICON.info}</span>
          <span
            ><strong
              >{endpointActive ? "Close endpoint" : "Endpoint info"}</strong
            ><small>Server and connection</small></span
          >
        </button>
      {/if}
      {#if showTheme}
        <button type="button" role="menuitem" onclick={() => choose(onTheme)}>
          <span>
            {#if theme === "light"}{@html ICON.sun}{:else if theme === "dark"}{@html ICON.moon}{:else}{@html ICON.contrast}{/if}
          </span>
          <span
            ><strong>Theme: {theme}</strong><small>Cycle appearance</small
            ></span
          >
        </button>
      {/if}
    </div>
  {/if}
</div>

<style>
  .more-control {
    position: relative;
  }
  .more-trigger {
    display: grid;
    place-items: center;
    width: 32px;
    height: 32px;
    padding: 0;
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-2);
    box-shadow: inset 0 1px 0 var(--edge-light);
    color: var(--text-muted);
    cursor: pointer;
  }
  .more-trigger:hover,
  .more-trigger[aria-expanded="true"] {
    border-color: var(--border-strong);
    color: var(--text);
  }
  .more-trigger.active {
    border-color: color-mix(in srgb, var(--brand) 62%, var(--border));
    background: var(--brand-soft);
    color: var(--brand-strong);
    box-shadow: inset 0 -2px 0 var(--brand);
  }
  .more-trigger :global(svg),
  .more-menu button > span:first-child :global(svg) {
    width: 16px;
    height: 16px;
  }
  .more-menu {
    position: absolute;
    top: calc(100% + 7px);
    right: 0;
    z-index: 50;
    width: 230px;
    padding: 5px;
    border: 1px solid var(--border-strong);
    border-radius: var(--r-chrome);
    background: var(--surface-1);
    box-shadow: var(--shadow-float);
  }
  .more-menu button {
    display: grid;
    grid-template-columns: 24px minmax(0, 1fr);
    align-items: center;
    gap: 8px;
    width: 100%;
    min-height: 48px;
    padding: 6px 8px;
    border: 0;
    border-radius: var(--r-well);
    background: transparent;
    color: var(--text-muted);
    text-align: left;
    cursor: pointer;
  }
  .more-menu button:hover,
  .more-menu button:focus-visible,
  .more-menu button[aria-current] {
    background: var(--brand-soft);
    color: var(--text);
  }
  .more-menu button > span:first-child {
    display: grid;
    place-items: center;
    color: var(--brand-strong);
  }
  .more-menu strong,
  .more-menu small {
    display: block;
  }
  .more-menu strong {
    font-size: var(--type-xs);
  }
  .more-menu small {
    margin-top: 2px;
    color: var(--text-soft);
    font-size: 9px;
  }
</style>
