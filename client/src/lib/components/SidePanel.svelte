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
        {#if kicker}<span class="kicker">{kicker}</span>{/if}
        <h2>{title}</h2>
      </div>
      <button
        class="close-btn"
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
    border: 0;
    padding: 0;
    background: color-mix(in srgb, var(--canvas) 55%, transparent);
    opacity: 0;
    pointer-events: none;
    transition: opacity var(--dur-slide) var(--ease-out);
  }
  .panel-layer.open .backdrop {
    opacity: 1;
    pointer-events: auto;
  }
  .panel-layer.docked .backdrop {
    display: none;
  }

  .panel {
    position: fixed;
    top: var(--topbar-h);
    bottom: var(--statusbar-h);
    width: var(--panel-w, min(480px, 92vw));
    z-index: 50;
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    background:
      linear-gradient(180deg, var(--surface-2), var(--surface-1) 32%),
      var(--surface-1);
    box-shadow: var(--shadow-float);
    padding: var(--space-4);
    pointer-events: auto;
    transition: transform var(--dur-slide) var(--ease-out);
  }
  .panel[data-side="right"] {
    right: 0;
    border-left: 1px solid var(--border-strong);
    transform: translateX(100%);
    grid-area: rightdock;
  }
  .panel[data-side="left"] {
    left: 0;
    border-right: 1px solid var(--border-strong);
    transform: translateX(-100%);
    grid-area: leftdock;
  }
  .panel-layer.open .panel {
    transform: translateX(0);
  }
  .panel-layer.raised:not(.docked) .panel {
    z-index: 51;
  }

  .panel-layer.docked .panel {
    position: relative;
    top: 0;
    bottom: auto;
    width: auto;
    height: 100%;
    transform: none;
    z-index: auto;
    box-shadow: none;
    transition: none;
  }

  .resize-handle {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 11px;
    z-index: 3;
    cursor: col-resize;
    touch-action: none;
  }
  .panel[data-side="left"] .resize-handle {
    right: -6px;
  }
  .panel[data-side="right"] .resize-handle {
    left: -6px;
  }
  .resize-handle::after {
    content: "";
    position: absolute;
    top: 0;
    bottom: 0;
    left: 50%;
    width: 2px;
    transform: translateX(-50%);
    background: transparent;
    transition: background var(--dur-hover) var(--ease-out);
  }
  .resize-handle:hover::after,
  .resize-handle:focus-visible::after {
    background: color-mix(in srgb, var(--brand) 65%, transparent);
  }
  .resize-handle:focus-visible {
    outline: none;
  }
  .panel-layer.docked:not(.open) .panel {
    display: none;
  }

  /* Only portrait phones use a bottom sheet. A short landscape phone remains
     a side flyout with its own scrollport, the same model as a tablet. */
  @media (max-width: 759px) and (orientation: portrait) {
    .panel-layer:not(.docked) .panel {
      top: auto;
      left: 0;
      right: 0;
      bottom: 0;
      width: 100%;
      height: 88dvh;
      border-radius: var(--r-well) var(--r-well) 0 0;
      padding-bottom: max(var(--space-4), env(safe-area-inset-bottom));
      transform: translateY(100%);
    }
    .panel-layer.open:not(.docked) .panel {
      transform: translateY(0);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .panel,
    .backdrop {
      transition: none;
    }
  }

  .sheet-handle {
    display: none;
    flex: 0 0 auto;
    width: 100%;
    height: 8px;
    align-items: start;
  }
  .sheet-grip {
    display: block;
    width: 36px;
    height: 4px;
    margin: -6px auto 0;
    border-radius: var(--r-full);
    background: var(--border-strong);
  }
  @media (max-width: 759px) and (orientation: portrait) {
    .panel-layer:not(.docked) .sheet-handle {
      display: flex;
      justify-content: center;
    }
  }

  .panel-head {
    flex: 0 0 auto;
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--space-3);
    min-width: 0;
  }
  .title {
    min-width: 0;
  }
  .title .kicker {
    color: var(--brand-strong);
    font-family: var(--font-mono);
    font-size: var(--type-xs);
    font-weight: 800;
    letter-spacing: 0.14em;
    text-transform: uppercase;
  }
  .title h2 {
    margin: 2px 0 0;
    font-family: var(--font-display);
    font-size: var(--type-xl);
    font-weight: 600;
    letter-spacing: var(--track-tight);
    color: var(--text);
    overflow-wrap: anywhere;
  }

  .close-btn {
    flex: none;
    display: grid;
    place-items: center;
    width: 34px;
    height: 34px;
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-inset);
    box-shadow: var(--elev-tile);
    color: var(--text-muted);
    cursor: pointer;
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

  .panel-toolbar {
    flex: 0 0 auto;
    min-width: 0;
  }

  .panel-body {
    flex: 1 1 auto;
    min-height: 0;
    min-width: 0;
    overflow-y: auto;
    overflow-x: hidden;
    overscroll-behavior: contain;
    touch-action: pan-y;
    -webkit-overflow-scrolling: touch;
    /* Overlay scrollbars sit over the scrollport in several engines. Reserve
       a physical inset so the thumb cannot cover cards or form controls. */
    padding-right: calc(var(--space-2) + 12px);
    scrollbar-gutter: stable;
    scrollbar-width: thin;
    scrollbar-color: var(--border-strong) transparent;
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }
  .panel-body::-webkit-scrollbar {
    width: 10px;
  }
  .panel-body::-webkit-scrollbar-track {
    background: transparent;
  }
  .panel-body::-webkit-scrollbar-thumb {
    border: 3px solid transparent;
    border-radius: var(--r-full);
    background: var(--border-strong);
    background-clip: padding-box;
  }
  .panel-body::-webkit-scrollbar-thumb:hover {
    background: var(--text-soft);
    background-clip: padding-box;
  }
</style>
