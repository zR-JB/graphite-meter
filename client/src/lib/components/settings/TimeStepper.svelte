<script lang="ts">
  import Roll from "../Roll.svelte";
  import { fmtStageTime, parseDuration } from "../../format";

  interface Props {
    /** What the time belongs to, as it reads mid-sentence: "Latency stage", "warmup". */
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
  // They round to the time the stepper shows, and the limits apply last so a server's is never passed.
  function commit(event: Event) {
    const input = event.currentTarget as HTMLInputElement;
    const typed = parseDuration(input.value);
    const next =
      typed === null ? ms : clamp(parseDuration(fmtStageTime(typed))!);
    input.value = fmtStageTime(next === ms || onChange(next) ? next : ms);
  }
  function onKey(event: KeyboardEvent) {
    const input = event.currentTarget as HTMLInputElement;
    // Escape drops a typed time; the sheet closes on the next one. A capture
    // listener sits on the field itself, so this runs before the sheet's own.
    if (event.key === "Escape" && input.value !== text) {
      input.value = text;
      event.preventDefault();
    }
    const direction = { ArrowUp: 1, ArrowDown: -1 }[event.key] as
      1 | -1 | undefined;
    if (!direction) return;
    event.preventDefault();
    nudge(direction);
  }
</script>

<!-- The field takes the keyboard, as a spin button does; − and + serve pointers and stay put at a limit. -->
<span class="stepper">
  <button
    type="button"
    class="btn btn-icon btn-quiet"
    tabindex="-1"
    aria-label="Shorten {label}"
    aria-disabled={ms <= min}
    {disabled}
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
      aria-label="{label[0].toUpperCase()}{label.slice(1)} time"
      aria-valuemin={min / 1000}
      aria-valuemax={max / 1000}
      aria-valuenow={ms / 1000}
      aria-valuetext={text}
      aria-invalid={ms > max}
      onchange={commit}
      onkeydowncapture={onKey}
    />
    <span class="shown" aria-hidden="true"><Roll {text} rank={ms} /></span>
  </span>
  <button
    type="button"
    class="btn btn-icon btn-quiet"
    tabindex="-1"
    aria-label="Lengthen {label}"
    aria-disabled={ms >= max}
    {disabled}
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
  /* The stepper grows as a whole for touch, so its buttons add no hit border;
     they sit in the track like segments, with concentric corners and an inset ring. */
  .stepper .btn {
    --hit-pad: 0px;
    width: var(--control-h);
    border-radius: calc(var(--r-chrome) - 2px);
    font: var(--w-normal) var(--type-lg) / 1 var(--font-sans);
  }
  .stepper .btn:focus-visible {
    outline-offset: -2px;
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
  /* The shown time sizes the field; the input only fills it, on top to take clicks and typing.
     While edited it is an ordinary focused field; otherwise only the rolled time shows. */
  .field input {
    z-index: 1;
    width: 0;
    min-width: 100%;
    height: var(--control-h);
    padding: 0 var(--space-1);
    border-radius: calc(var(--r-chrome) - 2px);
    caret-color: var(--text);
    text-align: center;
  }
  .field input:not(:focus-visible) {
    border-color: transparent;
    background: none;
    color: transparent;
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
