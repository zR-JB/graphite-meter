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

<label class="switch link-row" class:disabled>
  <input
    class="sr-only"
    type="checkbox"
    aria-describedby={tooltipText ? describedBy : undefined}
    {checked}
    {disabled}
    onchange={handleChange}
  />
  <span class="track" aria-hidden="true"><span class="knob"></span></span>
  <!-- Hidden, so the label alone names the switch; the tip is its description. -->
  {#if tooltipText}<span hidden id={describedBy}>{tooltipText}</span>{/if}
  {#if label}<span
      class="label"
      {@attach tooltipText ? tooltip(() => tooltipText) : null}>{label}</span
    >{/if}
</label>

<style>
  /* A plate row with the link row's wash and ring; it contains the hidden
     checkbox so focusing it cannot scroll the panel. */
  .switch {
    position: relative;
    display: flex;
    flex-direction: row-reverse;
    flex-wrap: nowrap;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
    user-select: none;
  }
  @media (pointer: coarse) {
    .switch {
      min-height: var(--hit);
    }
  }
  .switch.disabled {
    cursor: not-allowed;
  }
  /* On fills the track with ink, like a checked box; off is an empty track with the box's edge. */
  .track {
    position: relative;
    flex: none;
    width: 38px;
    height: 22px;
    border: var(--check-edge);
    border-radius: var(--r-full);
    background: var(--track);
    transition: var(--transition-control);
  }
  /* Centred on the track whatever its edge rounds to, 3 px from the end it rests at. */
  .knob {
    position: absolute;
    top: 50%;
    left: calc(50% - 16px);
    width: 16px;
    height: 16px;
    border-radius: var(--r-full);
    background: var(--surface-1);
    box-shadow: 0 1px 3px color-mix(in oklab, var(--shade) 40%, transparent);
    translate: 0 -50%;
    transition:
      translate var(--dur-graph) var(--ease-spring),
      background-color var(--dur-hover) var(--ease-out);
  }
  input:checked + .track {
    border-color: var(--brand);
    background: var(--brand);
  }
  input:checked + .track .knob {
    translate: 16px -50%;
    background: var(--text-inverse);
  }
  /* Forced colours keep the edge but drop fills; system colours draw the knob and the on state. */
  @media (forced-colors: active) {
    .knob {
      background: CanvasText;
    }
    input:checked + .track {
      background: Highlight;
    }
    input:checked + .track .knob {
      background: HighlightText;
    }
  }
  .label {
    flex: 0 1 auto;
    min-width: 0;
  }
</style>
