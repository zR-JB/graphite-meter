<script lang="ts">
  import Icon from "./Icon.svelte";
  import Dialog from "./Dialog.svelte";
  import { MIN_DOCK_WIDTH, MAX_DOCK_WIDTH } from "./dockWidths";
  import type { Snippet } from "svelte";
  import { sheetDrag } from "../actions/sheetDrag";
  import { tooltip } from "../actions/tooltip";
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
    onClose,
    children,
  }: Props = $props();

  let panelEl: HTMLElement | undefined;
  const flyoutWidth = $derived(
    Math.max(MIN_DOCK_WIDTH, Math.min(MAX_DOCK_WIDTH, preferredWidth)),
  );

  function setWidth(px: number) {
    onResize?.(Math.max(MIN_DOCK_WIDTH, Math.min(dockMaxWidth, px)));
  }

  function resizeHandle(handle: HTMLElement) {
    if (!open) return;
    let finish: (() => void) | undefined;
    const start = (event: PointerEvent) => {
      if (!event.isPrimary || event.button !== 0 || !panelEl) return;
      finish?.();
      event.preventDefault();
      const startX = event.clientX;
      const startWidth = panelEl.getBoundingClientRect().width;
      const { cursor, userSelect } = document.body.style;
      handle.setPointerCapture(event.pointerId);
      document.body.style.userSelect = "none";
      document.body.style.cursor = "col-resize";
      const move = (next: PointerEvent) => {
        if (next.pointerId === event.pointerId) {
          const delta = next.clientX - startX;
          setWidth(startWidth + (side === "left" ? delta : -delta));
        }
      };
      const end = (next: PointerEvent) => {
        if (next.pointerId === event.pointerId) finish?.();
      };
      finish = () => {
        finish = undefined;
        handle.removeEventListener("pointermove", move);
        handle.removeEventListener("pointerup", end);
        handle.removeEventListener("pointercancel", end);
        handle.removeEventListener("lostpointercapture", end);
        if (handle.hasPointerCapture(event.pointerId))
          handle.releasePointerCapture(event.pointerId);
        document.body.style.cursor = cursor;
        document.body.style.userSelect = userSelect;
      };
      handle.addEventListener("pointermove", move);
      handle.addEventListener("pointerup", end);
      handle.addEventListener("pointercancel", end);
      handle.addEventListener("lostpointercapture", end);
    };
    handle.addEventListener("pointerdown", start);
    return () => {
      finish?.();
      handle.removeEventListener("pointerdown", start);
    };
  }

  function onHandleKey(e: KeyboardEvent) {
    const width = panelEl?.offsetWidth ?? MIN_DOCK_WIDTH;
    const step = e.shiftKey ? 48 : 16;
    const next: Record<string, number> = {
      ArrowRight: width + step,
      ArrowUp: width + step,
      ArrowLeft: width - step,
      ArrowDown: width - step,
      Home: MIN_DOCK_WIDTH,
      End: dockMaxWidth,
    };
    if (e.key === "Enter" || e.key === " ") onResetWidth?.();
    else if (e.key in next) setWidth(next[e.key]);
    else return;
    e.preventDefault();
  }
</script>

<div class="panel-layer" class:docked style:--panel-w="{flyoutWidth}px">
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
      panelEl = node;
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
          {@attach tooltip(() => "Close (Esc)")}
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
        {@attach resizeHandle}
        onkeydown={onHandleKey}
        ondblclick={() => onResetWidth?.()}
      ></div>
    {/if}
  </Dialog>
</div>

<style>
  .panel-layer {
    display: contents;
  }
  .panel-layer > :global(dialog.panel) {
    max-width: none;
    max-height: none;
    margin: 0;
    flex-direction: column;
    border: 0 solid var(--border);
    border-inline-start-width: var(--hairline);
    background: var(--bg);
    color: var(--text);
  }
  .panel-layer > :global(dialog.panel.left) {
    border-inline-width: 0 var(--hairline);
  }
  .panel-layer > :global(dialog.panel[open]) {
    display: flex;
  }
  .docked > :global(dialog.panel) {
    grid-area: rightdock;
    position: relative;
    width: auto;
    height: 100%;
    background: transparent;
  }
  .docked > :global(dialog.panel.left) {
    grid-area: leftdock;
  }
  .panel-layer:not(.docked) > :global(dialog.panel) {
    --closed: translateX(100%);
    position: fixed;
    z-index: var(--z-panel);
    inset: var(--topbar-h) 0 var(--statusbar-h) auto;
    width: min(var(--panel-w), 100vw - var(--space-6));
    height: auto;
    box-shadow: var(--elev-float);
    transform: var(--closed);
    transition:
      transform var(--dur-slide) var(--ease-out),
      overlay var(--dur-slide) allow-discrete,
      display var(--dur-slide) allow-discrete;
  }
  .panel-layer:not(.docked) > :global(dialog.panel.left) {
    --closed: translateX(-100%);
    inset: var(--topbar-h) auto var(--statusbar-h) 0;
  }
  .panel-layer:not(.docked) > :global(dialog.panel[open]) {
    transform: none;
  }
  @starting-style {
    .panel-layer:not(.docked) > :global(dialog.panel[open]) {
      transform: var(--closed);
    }
  }
  .scrim {
    position: fixed;
    z-index: var(--z-scrim);
    inset: var(--topbar-h) 0 0;
    background: var(--scrim);
    opacity: 0;
    visibility: hidden;
    transition:
      opacity var(--dur-slide) var(--ease-out),
      visibility var(--dur-slide) allow-discrete;
  }
  .scrim.open {
    opacity: calc(1 - var(--sheet-drag, 0));
    visibility: visible;
  }

  .resize-handle {
    position: absolute;
    inset-block: 0;
    z-index: 3;
    width: 11px;
    cursor: col-resize;
    touch-action: none;
  }
  .resize-handle[data-side="left"] {
    right: -6px;
  }
  .resize-handle[data-side="right"] {
    left: -6px;
  }
  .resize-handle::after {
    content: "";
    position: absolute;
    inset-block: 0;
    left: 50%;
    width: 2px;
    translate: -50%;
    transition: background-color var(--dur-hover) var(--ease-out);
  }
  .resize-handle:is(:hover, :focus-visible)::after {
    background: var(--brand);
  }
  .resize-handle:focus-visible {
    outline: none;
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
  /* Short viewports (and 400% zoom) give the flyout the full height; the whole panel scrolls. */
  @media (max-height: 480px) {
    .panel-layer:not(.docked) > :global(dialog.panel:is(.left, .right)) {
      top: 0;
      bottom: 0;
      overflow-y: auto;
    }
    .panel-layer:not(.docked) .sheet-head {
      position: sticky;
      z-index: 1;
      top: 0;
      background: var(--canvas);
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
