<script lang="ts">
  import Dialog from "./Dialog.svelte";

  interface Props {
    open: boolean;
    id: string;
    title: string;
    description: string;
    confirmLabel: string;
    cancelLabel?: string;
    invoker?: HTMLElement | null;
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
    invoker,
    onConfirm,
    onCancel,
  }: Props = $props();
</script>

<Dialog
  {open}
  {invoker}
  {onCancel}
  role="alertdialog"
  labelledby={`${id}-title`}
  describedby={`${id}-description`}
>
  <div class="confirm">
    <h2 id={`${id}-title`}>{title}</h2>
    <p id={`${id}-description`}>{description}</p>
    <div class="confirm-actions">
      <button class="btn btn-inset" type="button" onclick={onCancel}>
        {cancelLabel}
      </button>
      <button
        class="btn btn-danger btn-solid"
        type="button"
        onclick={onConfirm}
      >
        {confirmLabel}
      </button>
    </div>
  </div>
</Dialog>

<style>
  .confirm {
    overflow-y: auto;
    padding: var(--space-4);
  }
  h2 {
    font: var(--w-strong) var(--type-lg) var(--font-display);
  }
  p {
    margin-top: var(--space-2);
    color: var(--text-muted);
    font-size: var(--type-sm);
    line-height: 1.45;
  }
  .confirm-actions {
    display: flex;
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: var(--space-2);
    margin-top: var(--space-4);
  }
</style>
