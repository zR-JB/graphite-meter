<script lang="ts">
  import Roll from "../Roll.svelte";
  import { fmtStageTime, parseDuration } from "../../format";

  interface Props {
    label: string;
    ms: number;
    min: number;
    max: number;
    /** The step size from `ms` in `direction`; steps grow with the time. */
    step: (ms: number, direction: 1 | -1) => number;
    disabled?: boolean;
    /** Applies a time; false when the run refuses it. */
    onChange: (ms: number) => boolean;
  }
  let {
    label,
    ms,
    min,
    max,
    step,
    disabled = false,
    onChange,
  }: Props = $props();
  const text = $derived(fmtStageTime(ms));
  const clamp = (value: number) => Math.min(max, Math.max(min, value));

  // A step lands on the step grid, so 10.5 s steps to 11 s or 10 s.
  function nudge(direction: 1 | -1) {
    const size = step(ms, direction);
    const next =
      direction > 0
        ? Math.floor(ms / size) * size + size
        : Math.ceil(ms / size) * size - size;
    onChange(clamp(next));
  }
  // Typed times read as seconds unless they name a unit: 90, 2.5, 2h, 1 h 30 min, 1:30:00.
  function commit(event: Event) {
    const input = event.currentTarget as HTMLInputElement;
    const typed = parseDuration(input.value);
    const next = typed === null ? ms : clamp(typed);
    if (next === ms || !onChange(next)) input.value = text;
  }
  function onKey(event: KeyboardEvent) {
    const direction = { ArrowUp: 1, ArrowDown: -1 }[event.key] as
      1 | -1 | undefined;
    if (!direction) return;
    event.preventDefault();
    nudge(direction);
  }
</script>

<span class="stepper" role="group" aria-label="{label} time">
  <button
    type="button"
    class="btn btn-icon btn-quiet"
    aria-label="Shorter {label}"
    disabled={disabled || ms <= min}
    onclick={() => nudge(-1)}>−</button
  >
  <span class="field">
    <input
      type="text"
      role="spinbutton"
      autocomplete="off"
      spellcheck="false"
      value={text}
      {disabled}
      aria-label="{label} time"
      aria-valuemin={min / 1000}
      aria-valuemax={max / 1000}
      aria-valuenow={ms / 1000}
      aria-valuetext={text}
      onchange={commit}
      onkeydown={onKey}
    />
    <span class="shown" aria-hidden="true"><Roll {text} rank={ms} /></span>
  </span>
  <button
    type="button"
    class="btn btn-icon btn-quiet"
    aria-label="Longer {label}"
    disabled={disabled || ms >= max}
    onclick={() => nudge(1)}>+</button
  >
</span>

<style>
  /* − time +: the time rolls like the strip's; a click edits it as text. */
  .stepper {
    --control-h: 28px;
    display: inline-grid;
    grid-template-columns: auto minmax(5.5rem, max-content) auto;
    align-items: center;
    padding: 2px;
    border-radius: var(--r-chrome);
    background: var(--track);
  }
  .stepper .btn {
    width: var(--control-h);
    font: var(--w-normal) var(--type-lg) / 1 var(--font-sans);
  }
  .field {
    display: grid;
    min-width: 0;
  }
  .field > * {
    grid-area: 1 / 1;
    align-self: center;
    text-align: center;
  }
  /* The input sits on top to take clicks and typing; its own text shows only while it is edited. */
  /* The shown time sizes the field; the input only fills it. */
  .field input {
    z-index: 1;
    width: 0;
    min-width: 100%;
    height: var(--control-h);
    padding: 0 var(--space-1);
    border: 0;
    border-radius: calc(var(--r-chrome) - 2px);
    background: none;
    box-shadow: none;
    color: transparent;
    caret-color: var(--text);
    text-align: center;
  }
  .field input:focus-visible {
    background: var(--surface-1);
    box-shadow: inset 0 0 0 var(--hairline) var(--field-edge);
    color: var(--text);
  }
  .field input:disabled {
    cursor: not-allowed;
  }
  .shown {
    font: var(--role-row);
    pointer-events: none;
  }
  .field:has(input:focus-visible) .shown {
    visibility: hidden;
  }
  .field:has(input:disabled) .shown {
    opacity: 0.5;
  }
  @media (pointer: coarse) {
    .stepper {
      --control-h: 40px;
    }
  }
</style>
