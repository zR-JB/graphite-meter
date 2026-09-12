<script lang="ts">
  import type { Snippet } from "svelte";
  import { ICON } from "../constants";
  let { label, children }: { label: string; children: Snippet } = $props();
  const id = $props.id();
  let trigger: HTMLButtonElement;
  let panel: HTMLDivElement;
  let close: HTMLButtonElement;
  let open = $state(false);
  function position() {
    const rect = trigger.getBoundingClientRect();
    const below = innerHeight - rect.bottom - 14;
    const above = rect.top - 14;
    panel.style.maxHeight = `${Math.max(80, Math.min(420, Math.max(below, above)))}px`;
    panel.style.left = `${Math.max(8, Math.min(rect.left, innerWidth - panel.offsetWidth - 8))}px`;
    panel.style.top = `${Math.max(8, below >= panel.offsetHeight || below >= above ? rect.bottom + 6 : rect.top - panel.offsetHeight - 6)}px`;
  }
  $effect(() => {
    if (!open) return;
    window.addEventListener("resize", position);
    document.addEventListener("scroll", position, true);
    return () => {
      window.removeEventListener("resize", position);
      document.removeEventListener("scroll", position, true);
    };
  });
</script>

<button
  bind:this={trigger}
  class="details-trigger"
  popovertarget={id}
  onclick={(event) => {
    event.preventDefault();
    if (panel.matches(":popover-open")) panel.hidePopover();
    else {
      trigger.focus({ preventScroll: true });
      panel.showPopover();
      position();
      close.focus({ preventScroll: true });
    }
  }}
>
  <span aria-hidden="true">{@html ICON.info}</span>{label}
</button>
<div
  bind:this={panel}
  class="diagnostic-details"
  ontoggle={(event) => {
    open = event.currentTarget.matches(":popover-open");
  }}
  {id}
  popover="auto"
  role="dialog"
  aria-labelledby={`${id}-title`}
>
  <header>
    <h3 id={`${id}-title`}>{label}</h3>
    <button
      bind:this={close}
      popovertarget={id}
      popovertargetaction="hide"
      aria-label="Close details">{@html ICON.close}</button
    >
  </header>
  <div class="details-body">{@render children()}</div>
</div>

<style>
  button {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    min-height: 32px;
    padding: 4px 8px;
    border: 1px solid var(--border);
    border-radius: var(--r-well);
    background: var(--surface-1);
    color: var(--text-muted);
    font: 500 var(--type-xs)/1.3 var(--font-sans);
    cursor: pointer;
  }
  button:hover {
    background: var(--brand-soft);
    color: var(--text);
  }
  button :global(svg) {
    width: 14px;
    height: 14px;
  }
  .details-trigger {
    white-space: nowrap;
  }
  .details-trigger span {
    display: inline-flex;
  }
  .diagnostic-details {
    position: fixed;
    inset: auto;
    width: min(360px, calc(100vw - 16px));
    max-height: min(420px, 70svh);
    overscroll-behavior: contain;
    margin: 0;
    padding: 0;
    overflow: auto;
    border: 1px solid var(--border-strong);
    border-radius: var(--r-chrome);
    background: var(--surface-2);
    color: var(--text);
    box-shadow: var(--shadow-float);
    font: var(--type-sm)/1.5 var(--font-sans);
  }
  header {
    position: sticky;
    top: 0;
    background: var(--surface-2);
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
    padding: var(--space-2) var(--space-3);
    border-bottom: 1px solid var(--border);
  }
  header button {
    border: 0;
    background: transparent;
  }
  h3 {
    margin: 0;
    font-size: var(--type-sm);
    font-weight: 600;
  }
  .details-body {
    padding: var(--space-3);
    overflow-wrap: anywhere;
  }
  @media (prefers-reduced-motion: no-preference) {
    .diagnostic-details:popover-open {
      animation: gm-reveal var(--dur-hover) var(--ease-out);
    }
  }
  @media (pointer: coarse) {
    button {
      min-height: 44px;
    }
  }
</style>
