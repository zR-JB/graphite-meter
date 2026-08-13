<script lang="ts">
  import { focusTrap } from "../actions/focusTrap";

  interface Props {
    open: boolean;
    id: string;
    title: string;
    description: string;
    confirmLabel: string;
    cancelLabel?: string;
    onConfirm: () => void;
    onCancel: () => void;
  }

  let {
    open,
    id,
    title,
    description,
    confirmLabel,
    cancelLabel = "Cancel",
    onConfirm,
    onCancel,
  }: Props = $props();
</script>

{#if open}
  <div class="confirm-backdrop">
    <div
      class="confirm-dialog"
      role="alertdialog"
      aria-modal="true"
      aria-labelledby={`${id}-title`}
      aria-describedby={`${id}-description`}
      tabindex="-1"
      use:focusTrap={true}
      onkeydown={(event) => {
        event.stopPropagation();
        if (event.key === "Escape") {
          event.preventDefault();
          onCancel();
        }
      }}
    >
      <h2 id={`${id}-title`}>{title}</h2>
      <p id={`${id}-description`}>{description}</p>
      <div class="confirm-actions">
        <button class="ghost-btn" type="button" onclick={onCancel}>
          {cancelLabel}
        </button>
        <button class="danger-btn" type="button" onclick={onConfirm}>
          {confirmLabel}
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  .confirm-backdrop {
    position: fixed;
    inset: 0;
    z-index: 180;
    display: grid;
    place-items: center;
    padding: var(--space-4);
    background: color-mix(in srgb, var(--canvas) 64%, transparent);
  }
  .confirm-dialog {
    width: min(360px, 100%);
    padding: var(--space-4);
    border: 1px solid var(--border-strong);
    border-radius: var(--r-chrome);
    background: var(--surface-1);
    box-shadow: var(--shadow-float);
  }
  .confirm-dialog h2 {
    margin: 0;
    font-family: var(--font-display);
    font-size: var(--type-lg);
    font-weight: 650;
    letter-spacing: 0;
  }
  .confirm-dialog p {
    margin: var(--space-2) 0 0;
    color: var(--text-muted);
    font-size: var(--type-sm);
    line-height: 1.45;
  }
  .confirm-actions {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-2);
    margin-top: var(--space-4);
  }
  .ghost-btn,
  .danger-btn {
    min-height: 32px;
    padding: 0 var(--space-3);
    border-radius: var(--r-chrome);
    font-size: var(--type-sm);
    font-weight: 650;
  }
  .ghost-btn {
    border: 1px solid var(--border);
    background: var(--surface-2);
    color: var(--text-muted);
  }
  .danger-btn {
    border: 1px solid color-mix(in srgb, var(--err) 55%, var(--border));
    background: var(--err-soft);
    color: var(--text);
  }
  .danger-btn:hover {
    border-color: var(--err);
  }
  button:focus-visible {
    outline: var(--focus-ring);
    outline-offset: 2px;
  }
</style>
