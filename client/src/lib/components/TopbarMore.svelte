<script lang="ts">
  import { tick } from "svelte";
  import { ICON } from "../constants";
  import type { ThemePref } from "../state/persistence";
  import MoreMenu from "./MoreMenu.svelte";

  interface Props {
    showHistory: boolean;
    showEndpoint: boolean;
    showTheme: boolean;
    historyActive: boolean;
    endpointActive: boolean;
    theme: ThemePref;
    onHistory: (invoker: HTMLElement) => void;
    onEndpoint: (invoker: HTMLElement) => void;
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

  async function chooseHistory(invoker: HTMLButtonElement) {
    onHistory(invoker);
    await tick();
    if (invoker.isConnected) invoker.focus({ preventScroll: true });
  }

  function chooseTheme(invoker: HTMLButtonElement) {
    invoker.focus({ preventScroll: true });
    onTheme();
  }
</script>

<MoreMenu label="More controls">
  {#snippet children(select)}
    {#if showHistory}
      <button
        type="button"
        role="menuitem"
        tabindex="-1"
        aria-current={historyActive ? "page" : undefined}
        onclick={() => select(chooseHistory)}
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
        tabindex="-1"
        aria-current={endpointActive ? "true" : undefined}
        onclick={() => select(onEndpoint)}
      >
        <span>{@html ICON.info}</span>
        <span
          ><strong>{endpointActive ? "Close endpoint" : "Endpoint info"}</strong
          ><small>Server and connection</small></span
        >
      </button>
    {/if}
    {#if showTheme}
      <button
        type="button"
        role="menuitem"
        tabindex="-1"
        onclick={() => select(chooseTheme)}
      >
        <span>
          {#if theme === "light"}{@html ICON.sun}{:else if theme === "dark"}{@html ICON.moon}{:else}{@html ICON.contrast}{/if}
        </span>
        <span
          ><strong>Theme: {theme}</strong><small>Cycle appearance</small></span
        >
      </button>
    {/if}
  {/snippet}
</MoreMenu>
