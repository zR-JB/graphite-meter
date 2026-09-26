<script lang="ts">
  // Action menu on a native auto popover: light dismiss, Escape and the top
  // layer come from the platform; the menu keeps roving arrow-key focus.
  import type { Snippet } from "svelte";
  import { focusMenuItem, navigateMenu } from "../actions/menu";
  import { tooltip } from "../actions/tooltip";
  import { ICON } from "../constants";

  interface Props {
    label: string;
    danger?: boolean;
    children: Snippet<
      [select: (action: (invoker: HTMLButtonElement) => void) => void]
    >;
  }
  let { label, danger = false, children }: Props = $props();
  let open = $state(false);
  let focusLast = false;
  let trigger: HTMLButtonElement;
  let menu: HTMLDivElement;
  const menuId = $props.id();

  function triggerKeydown(event: KeyboardEvent) {
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    event.preventDefault();
    event.stopPropagation();
    focusLast = event.key === "ArrowUp";
    menu.showPopover();
  }
  function select(action: (invoker: HTMLButtonElement) => void) {
    menu.hidePopover();
    action(trigger);
  }
</script>

<div class="more-control" class:danger>
  <button
    bind:this={trigger}
    class="btn btn-icon more-trigger"
    type="button"
    aria-label={label}
    aria-haspopup="menu"
    aria-expanded={open}
    aria-controls={menuId}
    popovertarget={menuId}
    style:anchor-name={`--${menuId}`}
    onkeydown={triggerKeydown}
    use:tooltip={{ text: label, disabled: !danger }}
  >
    {@html ICON.more}
  </button>
  <div
    bind:this={menu}
    id={menuId}
    class="float popover align-end menu more-menu"
    popover="auto"
    role="menu"
    tabindex="-1"
    aria-label={label}
    style:position-anchor={`--${menuId}`}
    ontoggle={(event) => {
      open = event.newState === "open";
      if (open) focusMenuItem(menu, focusLast);
      focusLast = false;
    }}
    onkeydown={(event) =>
      navigateMenu(event, menu, trigger, () => menu.hidePopover())}
  >
    {@render children(select)}
  </div>
</div>

<style>
  .more-menu {
    width: min(236px, calc(100vw - 16px));
  }
  .more-menu :global(button) {
    min-height: 48px;
  }
  .more-menu :global(:is(strong, small)) {
    display: block;
  }
  .more-menu :global(strong) {
    font-size: var(--type-xs);
  }
  .more-menu :global(small) {
    margin-top: 2px;
    color: var(--text-muted);
    font-size: var(--type-2xs);
  }
  .danger .more-menu :global(button) {
    color: var(--err);
  }
  .danger .more-menu :global(button:focus-visible) {
    background: var(--err-soft);
  }
  @media (hover: hover) {
    .danger .more-menu :global(button:hover) {
      background: var(--err-soft);
    }
  }
  .danger .more-menu :global(button > span:first-child) {
    color: inherit;
  }
  .danger .more-menu :global(strong) {
    color: var(--text);
  }
</style>
