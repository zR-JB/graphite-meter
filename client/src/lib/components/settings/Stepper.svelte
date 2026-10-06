<script lang="ts">
  import Roll from "../Roll.svelte";

  interface Props {
    /** What the value belongs to, as it reads mid-sentence: "Latency stage", "warmup", "stream limit". */
    label: string;
    value: number;
    min: number;
    max: number;
    /** The step size from `value` in `direction`. */
    step: (value: number, direction: 1 | -1) => number;
    format: (value: number) => string;
    /** A typed value, or null when the text is not one. */
    parse: (text: string) => number | null;
    /** The spoken value's unit in `value`'s: 1000 for milliseconds read as seconds. */
    unit?: number;
    /** The keys' verbs: "Shorten warmup", "Lengthen warmup". */
    verbs?: [string, string];
    /** What the field holds, after the label in its name: "Warmup time". */
    noun?: string;
    /** The field's least width. */
    fieldMin?: string;
    disabled?: boolean;
    /** Applies a value; false when the run refuses it. */
    onChange: (value: number) => boolean;
  }
  let {
    label,
    value,
    min,
    max,
    step,
    format,
    parse,
    unit = 1,
    verbs = ["Decrease", "Increase"],
    noun = "",
    fieldMin = "5.5rem",
    disabled = false,
    onChange,
  }: Props = $props();

  const text = $derived(format(value));
  const clamp = (next: number) => Math.min(max, Math.max(min, next));
  const HOLD_MS = 500;
  const REPEAT_MS = 150;
  let hold = 0;
  // A held key steps once, pauses, then repeats until it lifts or the value meets its limit.
  function press(event: PointerEvent, direction: 1 | -1) {
    if (event.button !== 0) return;
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
    nudge(direction);
    const tick = () => {
      if (direction > 0 ? value >= max : value <= min) return release();
      nudge(direction);
      // Not motion: a key held down repeats its step.
      hold = window.setTimeout(tick, REPEAT_MS);
    };
    // Not motion: a key held down repeats its step after a pause.
    hold = window.setTimeout(tick, HOLD_MS);
  }
  function release() {
    clearTimeout(hold);
    hold = 0;
  }
  // A keyboard's activation arrives as a click without a pointer; a pointer's click follows its press.
  const keyed = (event: MouseEvent, direction: 1 | -1) => {
    if (event.detail === 0) nudge(direction);
  };
  // A step lands on the step grid, so 10.5 s steps to 11 s or 10 s.
  function nudge(direction: 1 | -1) {
    const size = step(value, direction);
    const next =
      direction > 0
        ? Math.floor(value / size) * size + size
        : Math.ceil(value / size) * size - size;
    onChange(clamp(next));
  }
  // A typed value rounds to what the stepper would show, and the limits apply last so a server's is never passed.
  function commit(event: Event) {
    const input = event.currentTarget as HTMLInputElement;
    const typed = parse(input.value);
    const next = typed === null ? value : clamp(parse(format(typed)) ?? value);
    input.value = format(next === value || onChange(next) ? next : value);
  }
  function onKey(event: KeyboardEvent) {
    const input = event.currentTarget as HTMLInputElement;
    // Escape drops a typed value; the sheet closes on the next one. A capture listener sits on the field itself,
    // so this runs before the sheet's own.
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
<span class="stepper" style:--field-min={fieldMin}>
  <button
    type="button"
    class="btn btn-icon btn-quiet"
    tabindex="-1"
    aria-label="{verbs[0]} {label}"
    aria-disabled={value <= min}
    {disabled}
    onpointerdown={(event) => press(event, -1)}
    onpointerup={release}
    onpointercancel={release}
    onlostpointercapture={release}
    oncontextmenu={(event) => event.preventDefault()}
    onclick={(event) => keyed(event, -1)}>−</button
  >
  <span class="field">
    <input
      type="text"
      role="spinbutton"
      autocomplete="off"
      spellcheck="false"
      value={text}
      {disabled}
      aria-label="{label[0].toUpperCase()}{label.slice(1)}{noun
        ? ` ${noun}`
        : ''}"
      aria-valuemin={min / unit}
      aria-valuemax={max / unit}
      aria-valuenow={value / unit}
      aria-valuetext={text}
      aria-invalid={value > max}
      onchange={commit}
      onkeydowncapture={onKey}
    />
    <span class="shown" aria-hidden="true"><Roll {text} rank={value} /></span>
  </span>
  <button
    type="button"
    class="btn btn-icon btn-quiet"
    tabindex="-1"
    aria-label="{verbs[1]} {label}"
    aria-disabled={value >= max}
    {disabled}
    onpointerdown={(event) => press(event, 1)}
    onpointerup={release}
    onpointercancel={release}
    onlostpointercapture={release}
    oncontextmenu={(event) => event.preventDefault()}
    onclick={(event) => keyed(event, 1)}>+</button
  >
</span>

<style>
  /* − value +: the value rolls like the strip's; a click edits it as text. */
  .stepper {
    --control-h: 28px;
    display: inline-grid;
    grid-template-columns: auto minmax(var(--field-min), max-content) auto;
    align-items: center;
    padding: 2px;
    border-radius: var(--r-chrome);
    background: var(--track);
    box-shadow: var(--elev-recess);
  }
  /* The stepper grows as a whole for touch, so its buttons add no hit border;
     they sit in the track like segments, with concentric corners and an inset ring. */
  .stepper .btn {
    --hit-pad: 0px;
    width: var(--control-h);
    border-radius: calc(var(--r-chrome) - 2px);
    font: var(--w-normal) var(--type-lg) / 1 var(--font-sans);
    touch-action: manipulation;
    user-select: none;
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
  /* The shown value sizes the field; the input only fills it, on top to take clicks and typing.
     While edited it is an ordinary focused field; otherwise only the rolled value shows. */
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
