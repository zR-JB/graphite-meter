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
    toolbar,
    children,
  }: Props = $props();

  function close() {
    open = false;
  }
</script>

<div class="panel-layer" class:open aria-hidden={!open}>
  <button
    class="backdrop"
    aria-label={`Close ${title}`}
    tabindex={open ? 0 : -1}
    onclick={close}
  ></button>

  <div
    class="panel"
    data-side={side}
    style={width ? `--panel-w: ${width}` : undefined}
    role="dialog"
    aria-modal="true"
    aria-label={label ?? title}
    inert={!open}
    tabindex="-1"
    use:focusTrap={open}
    onkeydown={(e) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        close();
      }
    }}
  >
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
  /* The layer spans the viewport below the topbar so the topbar stays
     visible and clickable while a panel is open. */
  .panel-layer {
    position: fixed;
    inset: var(--topbar-h) 0 0 0;
    z-index: 50;
    pointer-events: none;
  }

  .backdrop {
    position: absolute;
    inset: 0;
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

  .panel {
    position: absolute;
    top: 0;
    bottom: 0;
    width: var(--panel-w, min(480px, 92vw));
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
  }
  .panel[data-side="left"] {
    left: 0;
    border-right: 1px solid var(--border-strong);
    transform: translateX(-100%);
  }
  .panel-layer.open .panel {
    transform: translateX(0);
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
    padding-right: var(--space-1);
    scrollbar-gutter: stable;
    display: flex;
    flex-direction: column;
    gap: var(--space-4);
  }
</style>
