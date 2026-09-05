<script lang="ts">
  import { tick, type Snippet } from "svelte";
  import { fromAction } from "svelte/attachments";
  import { focusMenuItem, navigateMenu } from "../actions/menu";
  import { tooltip } from "../actions/tooltip";
  import { ICON } from "../constants";

  interface Props {
    label: string;
    archive?: boolean;
    children: Snippet<
      [select: (action: (invoker: HTMLButtonElement) => void) => void]
    >;
  }
  let { label, archive = false, children }: Props = $props();
  let open = $state(false);
  let trigger = $state<HTMLButtonElement>();
  let menu = $state<HTMLDivElement>();
  const menuId = $props.id();

  async function show(last = false) {
    open = true;
    await tick();
    if (open) focusMenuItem(menu, last);
  }
  function triggerKeydown(event: KeyboardEvent) {
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    event.preventDefault();
    event.stopPropagation();
    void show(event.key === "ArrowUp");
  }
  function select(action: (invoker: HTMLButtonElement) => void) {
    open = false;
    if (trigger) action(trigger);
  }
  function outside(event: PointerEvent) {
    const target = event.target;
    if (!(target instanceof Node)) return;
    if (!trigger?.contains(target) && !menu?.contains(target)) open = false;
  }
</script>

<svelte:document onpointerdown={outside} />

<div class="more-control" class:archive>
  <button
    bind:this={trigger}
    class="more-trigger"
    type="button"
    aria-label={label}
    aria-haspopup="menu"
    aria-expanded={open}
    aria-controls={open ? menuId : undefined}
    onkeydown={triggerKeydown}
    onclick={() => {
      if (open) open = false;
      else void show();
    }}
    {@attach archive && fromAction(tooltip, () => label)}
  >
    {@html ICON.more}
  </button>
  {#if open}
    <div
      bind:this={menu}
      id={menuId}
      class="more-menu"
      role="menu"
      tabindex="-1"
      aria-label={label}
      onkeydown={(event) =>
        navigateMenu(event, menu, trigger, () => (open = false))}
    >
      {@render children(select)}
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
  .more-control :global(svg) {
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
  .more-menu :global(button) {
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
  .more-menu :global(button:hover),
  .more-menu :global(button:focus-visible),
  .more-menu :global(button[aria-current]) {
    background: var(--brand-soft);
    color: var(--text);
  }
  .more-menu :global(button > span:first-child) {
    display: grid;
    place-items: center;
    color: var(--brand-strong);
  }
  .more-menu :global(strong),
  .more-menu :global(small) {
    display: block;
  }
  .more-menu :global(strong) {
    font-size: var(--type-xs);
  }
  .more-menu :global(small) {
    margin-top: 2px;
    color: var(--text-muted);
    font-size: 9px;
  }
  .archive .more-trigger {
    width: 34px;
    height: 34px;
  }
  .archive :global(svg) {
    width: 15px;
    height: 15px;
  }
  .archive .more-menu {
    top: calc(100% + 6px);
    z-index: 31;
    width: min(238px, calc(100vw - 32px));
    transform-origin: top right;
  }
  .archive .more-menu :global(button) {
    grid-template-columns: 22px minmax(0, 1fr);
    min-height: 46px;
    color: var(--err);
  }
  .archive .more-menu :global(button:hover),
  .archive .more-menu :global(button:focus-visible) {
    background: var(--err-soft);
  }
  .archive .more-menu :global(button > span:first-child) {
    color: inherit;
  }
  .archive .more-menu :global(strong) {
    color: var(--text);
  }
  @media (prefers-reduced-motion: no-preference) {
    .archive .more-menu {
      animation: reveal-menu var(--dur-hover) var(--ease-out) both;
    }
    @keyframes reveal-menu {
      from {
        opacity: 0;
        transform: translateY(-3px) scale(0.985);
      }
    }
  }
  @media (pointer: coarse) {
    .more-trigger,
    .archive .more-trigger {
      width: 44px;
      height: 44px;
    }
  }
</style>
