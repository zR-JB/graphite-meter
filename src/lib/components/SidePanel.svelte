<script lang="ts">
  /* ============================================================
   * <SidePanel> — the shared flyout base for both auxiliary panels
   * (Workbench / Settings on the left, Connection & telemetry on the
   * right). One component so they look and behave identically:
   *  - opens BELOW the topbar over a shared backdrop (topbar stays
   *    visible + interactive), never over it
   *  - slides in/out from its side; stays mounted so the exit
   *    animation plays, `inert` keeps it out of the tab order closed
   *  - shared faceplate chrome, header (kicker + title + close),
   *    focus trap, and Esc-to-close
   * Content is provided via snippets: an optional `toolbar` row
   * (e.g. the Workbench tabs) pinned under the header, and `children`
   * which scroll in the body.
   * ============================================================ */
  import type { Snippet } from "svelte";
  import { focusTrap } from "../actions/focusTrap";
  import { ICON } from "../constants";

  interface Props {
    open?: boolean;
    side?: "left" | "right";
    title: string;
    kicker?: string;
    /** Accessible label for the dialog; defaults to the title. */
    label?: string;
    /** Optional width override (CSS length); defaults to the shared token. */
    width?: string;
    /** When true (wide screens) the panel docks in-flow instead of overlaying:
        no backdrop, non-modal, no focus trap — it's a persistent sidebar. */
    docked?: boolean;
    /** Current docked width (px) — used for the resize handle's aria value. */
    dockWidth?: number;
    /** Report a new docked width (px) as the inner edge is dragged. */
    onResize?: (px: number) => void;
    /** Reset the docked width to its default (double-click / Enter on handle). */
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
    dockWidth,
    onResize,
    onResetWidth,
    toolbar,
    children,
  }: Props = $props();

  function close() {
    open = false;
  }

  // ---- Docked resize (inner edge). Pointer-capture so the drag tracks
  // outside the thin handle; clamped; cleans up on up/cancel. ----
  const MIN_W = 320;
  function maxW() {
    return Math.round(Math.min(720, window.innerWidth * 0.6));
  }
  let panelEl = $state<HTMLDivElement>();

  function clamp(px: number) {
    return Math.max(MIN_W, Math.min(maxW(), px));
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
      // Left-docked grows as the pointer moves right; right-docked grows as it
      // moves left (the handle is always on the panel's inner edge).
      const delta = side === "left" ? ev.clientX - startX : startX - ev.clientX;
      onResize?.(clamp(startW + delta));
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
    // Map screen direction to "grow" depending on which edge the handle is on.
    const grow = side === "left" ? step : -step;
    onResize?.(clamp(panelEl.offsetWidth + grow));
  }
</script>

<div class="panel-layer" class:open class:docked aria-hidden={!open}>
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
        aria-orientation="vertical"
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
    <header class="panel-head">
      <div class="title">
        {#if kicker}<span class="kicker">{kicker}</span>{/if}
        <h2>{title}</h2>
      </div>
      <button
        class="close-btn"
        aria-label={`Close ${title}`}
        title="Close (Esc)"
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
  /* The layer is a logical grouping only (display:contents) so the panel and
     backdrop are direct children of the #console grid — the panel can then be
     placed into a reserved grid column when docked. Backdrop and panel both
     position themselves explicitly, so they don't depend on the layer box. */
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
  /* Docked = a persistent sidebar, never an overlay → no backdrop. */
  .panel-layer.docked .backdrop {
    display: none;
  }

  /* ---- Flyout (default): fixed overlay below the topbar, slides from side ---- */
  .panel {
    position: fixed;
    top: var(--topbar-h);
    bottom: 0;
    width: var(--panel-w, min(480px, 92vw));
    z-index: 50;
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    background: linear-gradient(180deg, var(--surface-2), var(--surface-1) 32%),
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

  /* ---- Docked: in-flow column in the #console grid, pushing the stage ---- */
  .panel-layer.docked .panel {
    position: relative; /* anchors the resize handle */
    /* Neutralize the flyout's fixed offsets — as a relative grid item the
       panel must sit flush in its cell (no top:topbar-h shift, which caused a
       gap below the topbar). */
    top: 0;
    bottom: auto;
    width: auto;
    height: 100%;
    transform: none;
    z-index: auto;
    box-shadow: none;
    transition: none;
  }

  /* Resize handle — straddles the panel's INNER edge (right edge for a
     left-docked panel, left edge for a right-docked one). A wide invisible
     hit area with a thin grip that shows on hover/focus/drag. */
  .resize-handle {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 11px;
    z-index: 3;
    cursor: col-resize;
    touch-action: none; /* let pointer drag own the gesture on touch */
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
  /* Closed while docked → fully removed so its grid column collapses. */
  .panel-layer.docked:not(.open) .panel {
    display: none;
  }

  @media (prefers-reduced-motion: reduce) {
    .panel,
    .backdrop {
      transition: none;
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
    /* Stop scroll chaining: reaching the panel's scroll boundary must not
       scroll the page behind it (which would lift the anchored status bar). */
    overscroll-behavior: contain;
    padding-right: var(--space-1);
    scrollbar-gutter: stable;
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }
</style>
