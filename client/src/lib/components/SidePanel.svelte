<script lang="ts">
  // Shared side panel primitive for Settings and telemetry: docked column on
  // wide layouts, focus-trapped flyout/sheet elsewhere.
  import type { Snippet } from "svelte";
  import { focusTrap } from "../actions/focusTrap";
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
    onResize?: (px: number) => void;
    onResetWidth?: () => void;
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
    onResize,
    onResetWidth,
    toolbar,
    children,
  }: Props = $props();

  function close() {
    open = false;
  }

  const MIN_W = 320;
  function maxW() {
    return Math.round(Math.min(720, window.innerWidth * 0.6));
  }
  let panelEl = $state<HTMLDivElement>();

  function clamp(px: number) {
    return Math.max(MIN_W, Math.min(maxW(), px));
  }

  function resizeBy(startWidth: number, delta: number) {
    onResize?.(clamp(startWidth + (side === "left" ? delta : -delta)));
  }

  function startResize(e: PointerEvent) {
    if (!docked || !panelEl) return;
    e.preventDefault();
    const handle = e.currentTarget as HTMLElement;
    const startX = e.clientX;
    const startW = panelEl.offsetWidth;
    handle.setPointerCapture(e.pointerId);
    document.body.style.userSelect = "none";
    document.body.style.cursor = "col-resize";

    const onMove = (ev: PointerEvent) => {
      resizeBy(startW, ev.clientX - startX);
    };
    const finish = () => {
      handle.removeEventListener("pointermove", onMove);
      handle.removeEventListener("pointerup", finish);
      handle.removeEventListener("pointercancel", finish);
      document.body.style.userSelect = "";
      document.body.style.cursor = "";
    };
    handle.addEventListener("pointermove", onMove);
    handle.addEventListener("pointerup", finish);
    handle.addEventListener("pointercancel", finish);
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

  const DISMISS_TAP_SLOP_PX = 8;
  const DISMISS_THRESHOLD_PX = 80;
  // Flyout sheets can be tapped or dragged downward to dismiss; docked panels
  // stay in-flow and use the close button instead.
  function startDismissDrag(e: PointerEvent) {
    if (docked || !panelEl) return;
    e.preventDefault();
    const handle = e.currentTarget as HTMLElement;
    const startY = e.clientY;
    handle.setPointerCapture(e.pointerId);
    panelEl.style.transition = "none";
    document.body.style.userSelect = "none";

    const onMove = (ev: PointerEvent) => {
      const delta = Math.max(0, ev.clientY - startY);
      panelEl!.style.transform = `translateY(${delta}px)`;
    };
    const finish = (ev: PointerEvent) => {
      handle.removeEventListener("pointermove", onMove);
      handle.removeEventListener("pointerup", finish);
      handle.removeEventListener("pointercancel", finish);
      document.body.style.userSelect = "";
      panelEl!.style.transition = "";
      panelEl!.style.transform = "";
      const delta = Math.max(0, ev.clientY - startY);
      if (
        ev.type !== "pointercancel" &&
        (delta <= DISMISS_TAP_SLOP_PX || delta > DISMISS_THRESHOLD_PX)
      ) {
        close();
      }
    };
    handle.addEventListener("pointermove", onMove);
    handle.addEventListener("pointerup", finish);
    handle.addEventListener("pointercancel", finish);
  }
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
    aria-label={`Close ${title}`}
    tabindex={open ? 0 : -1}
    onclick={close}
  ></button>

  <div
    class="panel"
    bind:this={panelEl}
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
        aria-valuemin={MIN_W}
        aria-valuemax={720}
        aria-valuenow={dockWidth}
        tabindex="0"
        onpointerdown={startResize}
        onkeydown={onHandleKey}
        ondblclick={() => onResetWidth?.()}
      ></div>
    {/if}
    <button
      class="sheet-handle"
      aria-label={`Drag down, or press Enter, to close ${title}`}
      onpointerdown={startDismissDrag}
      onclick={close}
    >
      <span class="sheet-grip" aria-hidden="true"></span>
    </button>
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
    bottom: 0;
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

  @media (max-width: 759px) {
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
    min-height: 52px;
    padding: 16px 0 12px;
    border: 0;
    background: transparent;
    cursor: grab;
    touch-action: none;
  }
  .sheet-handle:active {
    cursor: grabbing;
  }
  .sheet-grip {
    display: block;
    width: 36px;
    height: 4px;
    margin: 0 auto;
    border-radius: 999px;
    background: var(--border-strong);
  }
  @media (max-width: 759px) {
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
    padding-right: var(--space-1);
    scrollbar-gutter: stable;
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }
</style>
