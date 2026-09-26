<script lang="ts">
  import { tooltip } from "../actions/tooltip";

  interface Props {
    checked?: boolean;
    label?: string;
    disabled?: boolean;
    /** When given, the parent owns state and may veto the toggle. */
    onToggle?: (next: boolean) => void;
    /** Optional jargon-tooltip text for the label, e.g. JARGON.wireRate. */
    tooltip?: string;
  }
  let {
    checked = $bindable(false),
    label,
    disabled = false,
    onToggle,
    tooltip: tooltipText = "",
  }: Props = $props();

  function handleChange(e: Event) {
    const next = (e.currentTarget as HTMLInputElement).checked;
    if (onToggle) {
      // Controlled: revert the DOM to `checked` and let the parent decide.
      (e.currentTarget as HTMLInputElement).checked = checked;
      onToggle(next);
    } else {
      checked = next;
    }
  }
</script>

<label class="switch" class:disabled>
  <input
    class="sr-only"
    type="checkbox"
    {checked}
    {disabled}
    onchange={handleChange}
  />
  <span class="track" aria-hidden="true"><span class="knob"></span></span>
  {#if label}
    {#if tooltipText}
      <!-- The tooltip adds a tab stop, so only a label with text gets one. -->
      <span class="label term" use:tooltip={tooltipText}>{label}</span>
    {:else}
      <span class="label">{label}</span>
    {/if}
  {/if}
</label>

<style>
  /* Contains the hidden checkbox so focusing it cannot scroll the panel. */
  .switch {
    position: relative;
    display: inline-flex;
    align-items: center;
    gap: 10px;
    user-select: none;
  }
  .switch.disabled {
    cursor: not-allowed;
    opacity: 0.5;
  }
  .track {
    position: relative;
    flex: none;
    width: 36px;
    height: 20px;
    border: 1px solid var(--border);
    border-radius: var(--r-full);
    background: var(--surface-inset);
    transition: var(--transition-control);
  }
  .knob {
    position: absolute;
    top: 2px;
    left: 2px;
    width: 14px;
    height: 14px;
    border-radius: var(--r-full);
    background: var(--text-soft);
    transition:
      translate var(--dur-hover) var(--ease-snap),
      background-color var(--dur-hover) var(--ease-out);
  }
  input:checked + .track {
    border-color: var(--brand-line);
    background: var(--brand-soft);
  }
  input:checked + .track .knob {
    translate: 16px 0;
    background: var(--brand);
  }
  input:focus-visible + .track {
    outline: var(--focus-ring);
    outline-offset: 2px;
  }
  .label {
    flex: 1 1 auto;
    min-width: 0;
    font-size: var(--type-sm);
  }
</style>
