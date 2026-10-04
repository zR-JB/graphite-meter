<script lang="ts">
  interface Props {
    label: string;
    value: number;
    min: number;
    max: number;
    disabled?: boolean;
    /** Returns whether the value was taken. */
    onChange: (value: number) => boolean;
  }
  let { label, value, min, max, disabled = false, onChange }: Props = $props();
  const set = (next: number) => {
    const clamped = Math.min(max, Math.max(min, next));
    if (clamped !== value) onChange(clamped);
  };
</script>

<!-- − count +: a whole number stepped by its keys, the figure read rather than typed. -->
<span class="stepper" role="group" aria-label={label}>
  <button
    type="button"
    class="btn btn-icon btn-quiet"
    aria-label="Fewer {label}"
    disabled={disabled || value <= min}
    onclick={() => set(value - 1)}>−</button
  >
  <span class="count" class:dim={disabled}>{value}</span>
  <button
    type="button"
    class="btn btn-icon btn-quiet"
    aria-label="More {label}"
    disabled={disabled || value >= max}
    onclick={() => set(value + 1)}>+</button
  >
</span>

<style>
  .stepper {
    --control-h: 28px;
    display: inline-grid;
    grid-template-columns: auto 3ch auto;
    align-items: center;
    padding: 2px;
    border-radius: var(--r-chrome);
    background: var(--track);
  }
  .stepper .btn {
    --hit-pad: 0px;
    width: var(--control-h);
    border-radius: calc(var(--r-chrome) - 2px);
    font: var(--w-normal) var(--type-lg) / 1 var(--font-sans);
  }
  .stepper .btn:focus-visible {
    outline-offset: -2px;
  }
  .count {
    font: var(--role-figure);
    font-variant-numeric: tabular-nums;
    text-align: center;
  }
  .count.dim {
    opacity: 0.5;
  }
  @media (pointer: coarse) {
    .stepper {
      --control-h: 40px;
    }
  }
</style>
