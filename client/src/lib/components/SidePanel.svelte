<script lang="ts">
  import Icon from "./Icon.svelte";
  // Docked column on wide layouts, modal flyout or sheet elsewhere.
  import Dialog from "./Dialog.svelte";
  import { MIN_DOCK_WIDTH, MAX_DOCK_WIDTH } from "./dockWidths";
  import type { Snippet } from "svelte";
  import { sheetDrag } from "../actions/sheetDrag";
  import { tooltip } from "../actions/tooltip";

  interface Props {
    open: boolean;
    side?: "left" | "right";
    title: string;
    kicker?: string;
    docked?: boolean;
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
    kicker,
    docked = false,
    dockWidth,
    dockMaxWidth = MAX_DOCK_WIDTH,
    onResize,
    onResetWidth,
    onClose,
    children,
  }: Props = $props();

  let panelEl: HTMLElement | undefined;

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

<div class="panel-layer" class:docked>
  <Dialog
    {open}
    modal={!docked}
    onCancel={onClose}
    lightDismiss
    class="panel {side}"
    label={title}
    attach={(node) => {
      panelEl = node;
      if (open && !docked) return sheetDrag(onClose)(node);
    }}
  >
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
    <div class="sheet-handle" aria-hidden="true">
      <span class="sheet-grip" aria-hidden="true"></span>
    </div>
    <header class="panel-head">
      <div class="title">
        {#if kicker}<span class="caps">{kicker}</span>{/if}
        <h2>{title}</h2>
      </div>
      <button
        class="btn btn-icon btn-inset"
        aria-label={`Close ${title}`}
        use:tooltip={"Close (Esc)"}
        onclick={onClose}
      >
        <Icon name="close" />
      </button>
    </header>

    {#if open}
      <div class="panel-body">{@render children()}</div>
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
    gap: var(--space-3);
    padding: var(--space-4);
    border-left: 1px solid var(--border-strong);
    background: linear-gradient(180deg, var(--surface-2), var(--surface-1) 32%);
    color: var(--text);
  }
  .panel-layer > :global(dialog.panel.left) {
    border-left: 0;
    border-right: 1px solid var(--border-strong);
  }
  .panel-layer > :global(dialog.panel[open]) {
    display: flex;
  }
  .docked > :global(dialog.panel) {
    grid-area: rightdock;
    position: relative;
    width: auto;
    height: 100%;
  }
  .docked > :global(dialog.panel.left) {
    grid-area: leftdock;
  }
  .panel-layer:not(.docked) > :global(dialog.panel) {
    --closed: translateX(100%);
    position: fixed;
    inset: var(--topbar-h) 0 var(--statusbar-h) auto;
    width: min(440px, 92vw);
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
    width: min(560px, 94vw);
  }
  .panel-layer:not(.docked) > :global(dialog.panel[open]) {
    transform: none;
  }
  @starting-style {
    .panel-layer:not(.docked) > :global(dialog.panel[open]) {
      transform: var(--closed);
    }
  }
  .panel-layer > :global(dialog.panel::backdrop) {
    background: var(--scrim);
    opacity: calc(1 - var(--sheet-drag, 0));
    transition:
      opacity var(--dur-slide) var(--ease-out),
      overlay var(--dur-slide) allow-discrete,
      display var(--dur-slide) allow-discrete;
  }
  .panel-layer > :global(dialog.panel:not([open])::backdrop) {
    opacity: 0;
  }
  @starting-style {
    .panel-layer > :global(dialog.panel[open]::backdrop) {
      opacity: 0;
    }
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
    height: 8px;
  }
  .sheet-grip {
    width: 36px;
    height: 4px;
    margin-top: -6px;
    border-radius: var(--r-full);
    background: var(--border-strong);
  }
  /* Only portrait phones use a bottom sheet; landscape stays a side flyout. */
  @media (max-width: 759px) and (orientation: portrait) {
    .panel-layer:not(.docked) > :global(dialog.panel:is(.left, .right)) {
      --closed: translateY(100%);
      inset: auto 0 0;
      width: 100%;
      height: 88dvh;
      padding-bottom: max(var(--space-4), env(safe-area-inset-bottom));
      border-radius: var(--r-well) var(--r-well) 0 0;
    }
    .panel-layer:not(.docked) .sheet-handle {
      display: flex;
    }
  }

  .panel-head {
    display: flex;
    flex: none;
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--space-3);
    min-width: 0;
  }
  .title {
    min-width: 0;
  }
  .title .caps {
    color: var(--brand-strong);
    letter-spacing: var(--track-wide);
  }
  h2 {
    margin-top: 2px;
    font: var(--w-strong) var(--type-xl) var(--font-display);
    letter-spacing: var(--track-tight);
    overflow-wrap: anywhere;
  }
  .panel-body {
    display: flex;
    flex: 1 1 auto;
    flex-direction: column;
    gap: var(--space-4);
    min-width: 0;
    min-height: 0;
    overflow: hidden auto;
    overscroll-behavior: contain;
    touch-action: pan-y;
    /* Reserve room so overlay scrollbars cannot cover cards or controls. */
    padding-right: calc(var(--space-2) + 12px);
    scrollbar-gutter: stable;
    scrollbar-width: thin;
    scrollbar-color: var(--border-strong) transparent;
  }
</style>
