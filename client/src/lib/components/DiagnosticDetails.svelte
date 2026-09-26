<script lang="ts">
  // Anchored reading surface: a native auto popover supplies light dismiss,
  // Escape and focus return; CSS anchor positioning keeps it by its trigger.
  import type { Snippet } from "svelte";
  import { ICON } from "../constants";
  let { label, children }: { label: string; children: Snippet } = $props();
  const id = $props.id();
  let close: HTMLButtonElement;
</script>

<button
  class="btn details-trigger"
  type="button"
  popovertarget={id}
  style:anchor-name={`--${id}`}
>
  <span aria-hidden="true">{@html ICON.info}</span>{label}
</button>
<div
  class="float popover diagnostic-details"
  {id}
  popover="auto"
  role="dialog"
  aria-labelledby={`${id}-title`}
  style:position-anchor={`--${id}`}
  ontoggle={(event) => {
    if (event.newState === "open") close.focus({ preventScroll: true });
  }}
>
  <header>
    <h3 id={`${id}-title`}>{label}</h3>
    <button
      bind:this={close}
      class="btn btn-icon btn-quiet"
      type="button"
      popovertarget={id}
      popovertargetaction="hide"
      aria-label="Close details">{@html ICON.close}</button
    >
  </header>
  <div class="details-body">{@render children()}</div>
</div>

<style>
  .details-trigger :global(svg) {
    width: 14px;
    height: 14px;
  }
  .diagnostic-details {
    width: 360px;
    font: var(--type-sm) / 1.5 var(--font-sans);
  }
  header {
    position: sticky;
    top: 0;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
    padding: var(--space-1) var(--space-1) var(--space-1) var(--space-3);
    border-bottom: 1px solid var(--border);
    background: var(--surface-1);
  }
  h3 {
    font-size: var(--type-sm);
    font-weight: 600;
  }
  .details-body {
    padding: var(--space-3);
    overflow-wrap: anywhere;
  }
</style>
