<script lang="ts">
  /* ============================================================
   * <Switch> — toggle control (inherited component, §3.5)
   * Track + knob, brand accent when on. Native checkbox kept
   * for accessibility (visually hidden, focusable).
   * ============================================================ */
  interface Props {
    checked?: boolean;
    label?: string;
    disabled?: boolean;
    id?: string;
    /** Optional controlled handler. When provided, the parent owns state:
     *  the toggle is vetoable (e.g. live-toggle constraints) and `checked`
     *  is treated as a one-way input rather than a bound value. */
    onToggle?: (next: boolean) => void;
  }
  let {
    checked = $bindable(false),
    label,
    disabled = false,
    id,
    onToggle,
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
  <input type="checkbox" {checked} {disabled} {id} onchange={handleChange} />
  <span class="track" aria-hidden="true"><span class="knob"></span></span>
  {#if label}<span class="label">{label}</span>{/if}
</label>

<style>
  .switch {
    display: inline-flex;
    align-items: center;
    gap: 10px;
    cursor: pointer;
    user-select: none;
  }
  .switch.disabled {
    cursor: not-allowed;
    opacity: 0.5;
  }

  input {
    position: absolute;
    width: 1px;
    height: 1px;
    padding: 0;
    margin: -1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
    white-space: nowrap;
    border: 0;
  }

  .track {
    position: relative;
    flex: none;
    width: 36px;
    height: 20px;
    border-radius: 999px;
    background: var(--surface-inset);
    border: 1px solid var(--border);
    transition:
      background var(--dur-hover) var(--ease-out),
      border-color var(--dur-hover) var(--ease-out);
  }
  .knob {
    position: absolute;
    top: 50%;
    left: 2px;
    width: 14px;
    height: 14px;
    border-radius: 999px;
    background: var(--text-soft);
    transform: translateY(-50%);
    transition:
      transform var(--dur-hover) var(--ease-snap),
      background var(--dur-hover) var(--ease-out);
  }

  input:checked + .track {
    background: var(--brand-soft);
    border-color: color-mix(in srgb, var(--brand) 42%, var(--border));
  }
  input:checked + .track .knob {
    transform: translate(16px, -50%);
    background: var(--brand);
  }

  input:focus-visible + .track {
    outline: 2px solid color-mix(in srgb, var(--brand) 70%, transparent);
    outline-offset: 2px;
  }

  .label {
    font-size: 13px;
    color: var(--text);
  }
</style>
