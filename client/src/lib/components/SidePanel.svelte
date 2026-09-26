<script lang="ts">
  // Shared side panel primitive for Settings and telemetry: docked column on
  // wide layouts, focus-trapped flyout/sheet elsewhere.
  import { MIN_DOCK_WIDTH, MAX_DOCK_WIDTH } from "./dockWidths";
  import type { Snippet } from "svelte";
  import { focusTrap } from "../actions/focusTrap";
  import { sheetDrag } from "../actions/sheetDrag";
  import { ICON } from "../constants";
  import { tooltip } from "../actions/tooltip";

  interface Props {
    open?: boolean;
    side?: "left" | "right";
    title: string;
    kicker?: string;
    label?: string;
    width?: string;
    docked?: boolean;
    raised?: boolean;
    dockWidth?: number;
    dockMaxWidth?: number;
    onResize?: (px: number) => void;
    onResetWidth?: () => void;
    onClose?: () => void;
    toolbar?: Snippet;
    children: Snippet;
  }
  let {
    open = $bindable(false),
    side = "right",
    title,
    kicker,
    label,
    width,
    docked = false,
    raised = false,
    dockWidth,
    dockMaxWidth = MAX_DOCK_WIDTH,
    onResize,
    onResetWidth,
    onClose,
    toolbar,
    children,
  }: Props = $props();

  function close() {
    if (onClose) onClose();
    else open = false;
  }

  let panelEl = $state<HTMLDivElement>();

  function resizeBy(startWidth: number, delta: number) {
    const desired = startWidth + (side === "left" ? delta : -delta);
    onResize?.(Math.max(MIN_DOCK_WIDTH, Math.min(dockMaxWidth, desired)));
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
        if (next.pointerId === event.pointerId)
          resizeBy(startWidth, next.clientX - startX);
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
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      onResetWidth?.();
      return;
    }
    if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
    if (!panelEl) return;
    e.preventDefault();
    const step = (e.shiftKey ? 48 : 16) * (e.key === "ArrowRight" ? 1 : -1);
    resizeBy(panelEl.offsetWidth, step);
  }

  let backdropEl = $state<HTMLButtonElement>();
</script>

<div
  class="panel-layer"
  class:open
  class:docked
  class:raised
  aria-hidden={!open}
>
  <button
    class="backdrop"
    bind:this={backdropEl}
    aria-label={`Close ${title}`}
    tabindex={open ? 0 : -1}
    onclick={close}
  ></button>

  <div
    class="panel"
    bind:this={panelEl}
    use:sheetDrag={{
      enabled: open && !docked,
      backdrop: backdropEl,
      onDismiss: close,
    }}
    data-side={side}
    style={width ? `--panel-w: ${width}` : undefined}
    role={docked ? "region" : "dialog"}
    aria-modal={docked ? undefined : true}
    aria-label={label ?? title}
    inert={!open}
    tabindex="-1"
    use:focusTrap={open && !docked}
    onkeydown={(e) => {
      if (e.key === "Escape") {
        if (document.querySelector(":popover-open")) return;
        e.stopPropagation();
        close();
      }
    }}
  >
    {#if docked}
      <div
        class="resize-handle"
        role="slider"
        aria-orientation="horizontal"
        aria-label={`Resize ${title} panel (arrow keys; Enter to reset)`}
        aria-valuemin={MIN_DOCK_WIDTH}
        aria-valuemax={dockMaxWidth}
        aria-valuenow={dockWidth}
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
        onclick={close}
      >
        {@html ICON.close}
      </button>
    </header>

    {#if toolbar}
      <div class="panel-toolbar">{@render toolbar()}</div>
    {/if}

    <div class="panel-body">{@render children()}</div>
  </div>
</div>

<style>
  .panel-layer {
    display: contents;
  }
  .backdrop {
    position: fixed;
    inset: var(--topbar-h) 0 0 0;
    z-index: 49;
    background: color-mix(in srgb, var(--canvas) 55%, transparent);
    opacity: 0;
    pointer-events: none;
    transition: opacity var(--dur-slide) var(--ease-out);
  }
  .open .backdrop {
    opacity: 1;
    pointer-events: auto;
  }
  .docked .backdrop {
    display: none;
  }

  .panel {
    position: fixed;
    top: var(--topbar-h);
    bottom: var(--statusbar-h);
    z-index: 50;
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    width: var(--panel-w, min(480px, 92vw));
    padding: var(--space-4);
    background: linear-gradient(180deg, var(--surface-2), var(--surface-1) 32%);
    box-shadow: var(--elev-float);
    transition: transform var(--dur-slide) var(--ease-out);
  }
  .panel[data-side="right"] {
    grid-area: rightdock;
    right: 0;
    border-left: 1px solid var(--border-strong);
    transform: translateX(100%);
  }
  .panel[data-side="left"] {
    grid-area: leftdock;
    left: 0;
    border-right: 1px solid var(--border-strong);
    transform: translateX(-100%);
  }
  .open .panel {
    transform: none;
  }
  .raised:not(.docked) .panel {
    z-index: 51;
  }
  .docked .panel {
    position: relative;
    inset: auto;
    z-index: auto;
    width: auto;
    height: 100%;
    box-shadow: none;
    transform: none;
    transition: none;
  }
  .docked:not(.open) .panel {
    display: none;
  }

  .resize-handle {
    position: absolute;
    inset-block: 0;
    z-index: 3;
    width: 11px;
    cursor: col-resize;
    touch-action: none;
  }
  [data-side="left"] .resize-handle {
    right: -6px;
  }
  [data-side="right"] .resize-handle {
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
    background: color-mix(in srgb, var(--brand) 65%, transparent);
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
  /* Only portrait phones use a bottom sheet. A short landscape phone remains
     a side flyout with its own scrollport, the same model as a tablet. */
  @media (max-width: 759px) and (orientation: portrait) {
    .panel-layer:not(.docked) .panel {
      inset: auto 0 0;
      width: 100%;
      height: 88dvh;
      padding-bottom: max(var(--space-4), env(safe-area-inset-bottom));
      border-radius: var(--r-well) var(--r-well) 0 0;
      transform: translateY(100%);
    }
    .open:not(.docked) .panel {
      transform: none;
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
    letter-spacing: 0.14em;
  }
  h2 {
    margin-top: 2px;
    font: 600 var(--type-xl) var(--font-display);
    letter-spacing: var(--track-tight);
    overflow-wrap: anywhere;
  }
  .panel-toolbar {
    flex: none;
    min-width: 0;
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
    /* Overlay scrollbars sit over the scrollport in several engines; reserve
       an inset so the thumb cannot cover cards or form controls. */
    padding-right: calc(var(--space-2) + 12px);
    scrollbar-gutter: stable;
    scrollbar-width: thin;
    scrollbar-color: var(--border-strong) transparent;
  }
</style>
