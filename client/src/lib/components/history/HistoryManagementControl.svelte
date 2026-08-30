<script lang="ts">
  import { onMount, tick } from "svelte";
  import { tooltip } from "../../actions/tooltip";
  import { ICON } from "../../constants";

  interface Props {
    onClear: (invoker: HTMLElement) => void;
  }

  let { onClear }: Props = $props();
  let open = $state(false);
  let trigger = $state<HTMLButtonElement>();
  let menu = $state<HTMLDivElement>();

  async function toggle() {
    open = !open;
    if (!open) return;
    await tick();
    menu?.querySelector<HTMLButtonElement>("button")?.focus();
  }

  function close(restoreFocus = false) {
    open = false;
    if (restoreFocus)
      void tick().then(() => trigger?.focus({ preventScroll: true }));
  }

  function requestClear() {
    if (!trigger) return;
    open = false;
    onClear(trigger);
  }

  onMount(() => {
    const outside = (event: PointerEvent) => {
      const target = event.target;
      if (!(target instanceof Node)) return;
      if (!trigger?.contains(target) && !menu?.contains(target)) close();
    };
    document.addEventListener("pointerdown", outside);
    return () => document.removeEventListener("pointerdown", outside);
  });
</script>

<div class="management-control">
  <button
    bind:this={trigger}
    class="management-trigger"
    type="button"
    aria-label="Archive management"
    aria-haspopup="menu"
    aria-expanded={open}
    use:tooltip={"Archive management"}
    onclick={toggle}
  >
    {@html ICON.more}
  </button>
  {#if open}
    <div
      bind:this={menu}
      class="management-menu"
      role="menu"
      aria-label="Archive management"
      tabindex="-1"
      onkeydown={(event) => {
        if (event.key !== "Escape") return;
        event.stopPropagation();
        event.preventDefault();
        close(true);
      }}
    >
      <button type="button" role="menuitem" onclick={requestClear}>
        <span>{@html ICON.trash}</span>
        <span
          ><strong>Clear all saved results</strong><small
            >Requires confirmation</small
          ></span
        >
      </button>
    </div>
  {/if}
</div>

<style>
  .management-control {
    position: relative;
  }
  .management-trigger {
    display: grid;
    place-items: center;
    width: 34px;
    height: 34px;
    padding: 0;
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-2);
    box-shadow: inset 0 1px 0 var(--edge-light);
    color: var(--text-muted);
    cursor: pointer;
  }
  .management-trigger:hover,
  .management-trigger[aria-expanded="true"] {
    border-color: var(--border-strong);
    color: var(--text);
  }
  .management-trigger :global(svg),
  .management-menu :global(svg) {
    width: 15px;
    height: 15px;
  }
  .management-menu {
    position: absolute;
    top: calc(100% + 6px);
    right: 0;
    z-index: 31;
    width: min(238px, calc(100vw - 32px));
    padding: 5px;
    border: 1px solid var(--border-strong);
    border-radius: var(--r-chrome);
    background: var(--surface-1);
    box-shadow: var(--shadow-float);
    transform-origin: top right;
  }
  .management-menu button {
    display: grid;
    grid-template-columns: 22px minmax(0, 1fr);
    align-items: center;
    gap: 8px;
    width: 100%;
    min-height: 46px;
    padding: 6px 8px;
    border: 0;
    border-radius: var(--r-well);
    background: transparent;
    color: var(--err);
    text-align: left;
    cursor: pointer;
  }
  .management-menu button:hover,
  .management-menu button:focus-visible {
    background: var(--err-soft);
  }
  .management-menu button > span:first-child {
    display: grid;
    place-items: center;
  }
  .management-menu strong,
  .management-menu small {
    display: block;
  }
  .management-menu strong {
    color: var(--text);
    font-size: var(--type-xs);
  }
  .management-menu small {
    margin-top: 2px;
    color: var(--text-muted);
    font-size: 9px;
  }
  @media (prefers-reduced-motion: no-preference) {
    .management-menu {
      animation: reveal-management var(--dur-hover) var(--ease-out) both;
    }
    @keyframes reveal-management {
      from {
        opacity: 0;
        transform: translateY(-3px) scale(0.985);
      }
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .management-menu {
      animation: none;
    }
  }
</style>
