<script lang="ts">
  import { tooltip } from "../actions/tooltip";

  interface Props {
    checked: boolean;
    label?: string;
    disabled?: boolean;
    onToggle: (next: boolean) => void;
    tooltip?: string;
  }
  let {
    checked,
    label,
    disabled = false,
    onToggle,
    tooltip: tooltipText = "",
  }: Props = $props();

  const uid = $props.id();
  const describedBy = `${uid}-tip`;

  function handleChange(e: Event & { currentTarget: HTMLInputElement }) {
    const next = e.currentTarget.checked;
    e.currentTarget.checked = checked;
    onToggle(next);
  }
</script>

<label class="switch" class:disabled>
  <input
    class="sr-only"
    type="checkbox"
    aria-describedby={tooltipText ? describedBy : undefined}
    {checked}
    {disabled}
    onchange={handleChange}
  />
  <span class="track" aria-hidden="true"><span class="knob"></span></span>
  {#if tooltipText}<span class="sr-only" id={describedBy}>{tooltipText}</span
    >{/if}
  {#if label}<span
      class="label"
      {@attach tooltipText ? tooltip(() => tooltipText) : null}>{label}</span
    >{/if}
</label>

<style>
  /* Contains the hidden checkbox so focusing it cannot scroll the panel. */
  .switch {
    position: relative;
    display: flex;
    flex-direction: row-reverse;
    flex-wrap: nowrap;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
    min-height: var(--control-h);
    border-radius: var(--r-well);
    user-select: none;
  }
  @media (pointer: coarse) {
    .switch {
      min-height: var(--hit);
    }
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
    border-radius: var(--r-full);
    background: var(--track);
    transition: var(--transition-control);
  }
  .knob {
    position: absolute;
    top: 2px;
    left: 2px;
    width: 16px;
    height: 16px;
    border-radius: var(--r-full);
    background: var(--text-soft);
    box-shadow: 0 1px 2px color-mix(in oklab, var(--shade) 30%, transparent);
    transition:
      translate var(--dur-hover) var(--ease-snap),
      background-color var(--dur-hover) var(--ease-out);
  }
  input:checked + .track {
    background: var(--selected-wash);
  }
  input:checked + .track .knob {
    translate: 16px 0;
    background: var(--brand);
  }
  .label {
    flex: 0 1 auto;
    min-width: 0;
  }
</style>
