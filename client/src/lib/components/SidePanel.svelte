<script lang="ts">
  import Icon from "./Icon.svelte";
  import Dialog from "./Dialog.svelte";
  import { MIN_DOCK_WIDTH, MAX_DOCK_WIDTH } from "./dockWidths";
  import type { Snippet } from "svelte";
  import { resize } from "../actions/resize";
  import { sheetDrag } from "../actions/sheetDrag";
  import { keyHint, tooltip } from "../actions/tooltip";
  import { activeModal } from "../actions/focus";

  interface Props {
    open: boolean;
    side?: "left" | "right";
    title: string;
    docked?: boolean;
    preferredWidth: number;
    dockWidth?: number;
    dockMaxWidth?: number;
    onResize?: (px: number) => void;
    onResetWidth?: () => void;
    /** A pointer drag on the handle starts or ends. */
    onResizing?: (dragging: boolean) => void;
    onClose: () => void;
    children: Snippet;
  }
  let {
    open,
    side = "right",
    title,
    docked = false,
    preferredWidth,
    dockWidth,
    dockMaxWidth = MAX_DOCK_WIDTH,
    onResize,
    onResetWidth,
    onResizing,
    onClose,
    children,
  }: Props = $props();

  const flyoutWidth = $derived(
    Math.max(MIN_DOCK_WIDTH, Math.min(MAX_DOCK_WIDTH, preferredWidth)),
  );
  // A closing sheet keeps the width its column gave it, so it leaves as it stood rather than at its preferred width.
  let settledDockWidth = $state(0);
  $effect(() => {
    if (dockWidth) settledDockWidth = dockWidth;
  });
  const sheetWidth = $derived(dockWidth || settledDockWidth || flyoutWidth);
</script>

<div
  class="panel-layer"
  class:docked
  style:--panel-w="{flyoutWidth}px"
  style:--dock-w="{sheetWidth}px"
>
  {#if !docked}<button
      class="scrim"
      class:open
      type="button"
      tabindex="-1"
      aria-hidden="true"
      onclick={onClose}
    ></button>{/if}
  <Dialog
    {open}
    modal={false}
    onCancel={onClose}
    class="panel sheet {side}"
    label={title}
    attach={(node) => {
      // Escape closes the panel holding focus; a modal on top or an open popover keeps it.
      const escape = (event: KeyboardEvent) => {
        if (event.key !== "Escape" || event.defaultPrevented || activeModal())
          return;
        if (document.querySelector(":popover-open:not(.tooltip)")) return;
        event.preventDefault();
        onClose();
      };
      node.addEventListener("keydown", escape);
      const drag = open && !docked ? sheetDrag(onClose)(node) : undefined;
      return () => {
        node.removeEventListener("keydown", escape);
        drag?.();
      };
    }}
  >
    <div class="sheet-handle" aria-hidden="true">
      <span class="sheet-grip" aria-hidden="true"></span>
    </div>
    <header class="sheet-head">
      <!-- svelte-ignore a11y_autofocus -->
      <h2 tabindex="-1" autofocus>{title}</h2>
      <div class="head-actions">
        <button
          class="btn btn-icon btn-quiet"
          aria-label={`Close ${title}`}
          {@attach tooltip(() => `Close${keyHint("Esc")}`)}
          onclick={onClose}
        >
          <Icon name="close" />
        </button>
      </div>
    </header>

    <div class="panel-body sheet-body">{@render children()}</div>
    <!-- After the content, so Tab from the title reaches the controls first. -->
    {#if docked}
      <div
        class="resize-handle"
        data-side={side}
        role="slider"
        aria-orientation="horizontal"
        aria-label={`Resize ${title} panel (arrow keys; Enter to reset)`}
        aria-valuemin={MIN_DOCK_WIDTH}
        aria-valuemax={dockMaxWidth}
        aria-valuenow={dockWidth}
        aria-valuetext={`${dockWidth} pixels wide`}
        tabindex="0"
        {@attach open &&
          resize({
            side,
            width: () => dockWidth ?? MIN_DOCK_WIDTH,
            min: MIN_DOCK_WIDTH,
            max: () => dockMaxWidth,
            set: (px) => onResize?.(px),
            reset: () => onResetWidth?.(),
            active: (dragging) => onResizing?.(dragging),
          })}
      ></div>
    {/if}
  </Dialog>
</div>

<style>
  .panel-layer {
    display: contents;
  }
  /* A sheet floats over the page: inset, rounded, frosted, with the one shadow the design allows. */
  .panel-layer > :global(dialog.panel) {
    max-width: none;
    max-height: none;
    margin: 0;
    flex-direction: column;
    overflow: hidden;
    border: var(--hairline) solid var(--border-subtle);
    border-radius: var(--r-surface);
    background: var(--sheet);
    -webkit-backdrop-filter: var(--sheet-blur);
    backdrop-filter: var(--sheet-blur);
    box-shadow: var(--elev-float);
    color: var(--text);
  }
  .panel-layer > :global(dialog.panel[open]) {
    display: flex;
  }
  /* Docked, nothing clips the resize handle that straddles the edge; the body clips its own corners. */
  .docked > :global(dialog.panel) {
    overflow: visible;
  }
  .docked .panel-body {
    border-radius: 0 0 calc(var(--r-surface) - var(--hairline))
      calc(var(--r-surface) - var(--hairline));
  }
  /* Docked, the sheet keeps its width and hugs its column's inner edge, so the column's glide is its slide:
     the sheet and the page it makes room in move in the same layout pass, with no second animation to
     fall behind on a slow machine. Closed, the column is 0 wide and the sheet hangs off the viewport's
     edge; it stays displayed for the glide out, then closes. */
  .docked > :global(dialog.panel) {
    grid-area: rightdock;
    justify-self: start;
    position: relative;
    width: calc(var(--dock-w) - var(--space-3));
    height: auto;
    margin: 0 var(--space-3) var(--space-3) 0;
    /* Docked, only the plain page lies behind it: a blur would cost every frame and change nothing. */
    -webkit-backdrop-filter: none;
    backdrop-filter: none;
    transition:
      overlay var(--dur-sheet) allow-discrete,
      display var(--dur-sheet) allow-discrete;
  }
  .docked > :global(dialog.panel.left) {
    grid-area: leftdock;
    justify-self: end;
    margin: 0 0 var(--space-3) var(--space-3);
  }
  .panel-layer:not(.docked) > :global(dialog.panel) {
    --closed: translateX(calc(100% + var(--space-4)));
    position: fixed;
    z-index: var(--z-panel);
    inset: var(--topbar-h) var(--space-2) var(--space-2) auto;
    width: min(var(--panel-w), 100vw - var(--space-6));
    height: auto;
    box-shadow: var(--elev-float);
    transform: var(--closed);
    transition:
      transform var(--dur-sheet) var(--ease-out),
      overlay var(--dur-sheet) allow-discrete,
      display var(--dur-sheet) allow-discrete;
  }
  .panel-layer:not(.docked) > :global(dialog.panel.left) {
    --closed: translateX(calc(-100% - var(--space-4)));
    inset: var(--topbar-h) auto var(--space-2) var(--space-2);
  }
  .panel-layer:not(.docked) > :global(dialog.panel[open]) {
    transform: none;
  }
  @starting-style {
    .panel-layer:not(.docked) > :global(dialog.panel[open]) {
      transform: var(--closed);
    }
  }
  /* The scrim comes and goes with the flyout it shades, on the sheet's own clock. */
  .scrim {
    position: fixed;
    z-index: var(--z-scrim);
    inset: var(--topbar-h) 0 0;
    background: var(--scrim);
    opacity: 0;
    visibility: hidden;
    transition:
      opacity var(--dur-sheet) var(--ease-out),
      visibility var(--dur-sheet) allow-discrete;
  }
  .scrim.open {
    opacity: calc(1 - var(--sheet-drag, 0));
    visibility: visible;
  }

  .resize-handle[data-side="left"] {
    right: -6px;
  }
  .resize-handle[data-side="right"] {
    left: -6px;
  }

  .sheet-handle {
    display: none;
    flex: none;
    justify-content: center;
    padding-top: 6px;
  }
  .sheet-grip {
    width: 36px;
    height: 4px;
    border-radius: var(--r-full);
    background: var(--border-strong);
  }
  @media (max-width: 759px) and (orientation: portrait) {
    .panel-layer:not(.docked) > :global(dialog.panel:is(.left, .right)) {
      --closed: translateY(100%);
      inset: auto 0 0;
      width: 100%;
      height: 88dvh;
      padding-bottom: env(safe-area-inset-bottom);
      border-width: var(--hairline) 0 0;
      border-radius: var(--r-surface) var(--r-surface) 0 0;
    }
    .panel-layer:not(.docked) .sheet-handle {
      display: flex;
    }
  }
  /* Short viewports (and 400% zoom) give the flyout the full height, inset as at its side; the whole panel scrolls. */
  @media (max-height: 480px) {
    .panel-layer:not(.docked) > :global(dialog.panel:is(.left, .right)) {
      top: var(--space-2);
      bottom: var(--space-2);
      overflow-y: auto;
    }
    .panel-layer:not(.docked) .sheet-head {
      position: sticky;
      z-index: 1;
      top: 0;
      background: var(--sheet-solid);
    }
    .panel-layer:not(.docked) .panel-body {
      flex: none;
      overflow: visible;
    }
  }

  .panel-body {
    flex: 1 1 auto;
    touch-action: pan-y;
  }
</style>
