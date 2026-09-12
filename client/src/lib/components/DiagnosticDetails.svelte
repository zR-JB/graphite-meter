<script lang="ts">
  import type { Snippet } from "svelte";
  import { ICON } from "../constants";
  let { label, children }: { label: string; children: Snippet } = $props();
  const id = $props.id();
</script>

<button class="details-trigger" popovertarget={id}>
  <span aria-hidden="true">{@html ICON.info}</span>{label}
</button>
<div
  class="diagnostic-details"
  {id}
  popover="auto"
  role="dialog"
  aria-labelledby={`${id}-title`}
>
  <header>
    <h3 id={`${id}-title`}>{label}</h3>
    <button
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
    inset: auto 12px 48px auto;
    width: min(400px, calc(100vw - 24px));
    max-height: min(480px, 60svh);
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
