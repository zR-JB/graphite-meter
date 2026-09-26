<script lang="ts">
  // Modal shell over native <dialog>: showModal() supplies the top layer,
  // background inertness, the focus trap and Escape. The owner keeps the
  // open state; every native close is reported through onCancel.
  import { tick, type Snippet } from "svelte";
  import { canFocus, hasFocus } from "../actions/focus";

  interface Props {
    open: boolean;
    onCancel: () => void;
    labelledby: string;
    describedby?: string;
    role?: "dialog" | "alertdialog";
    /** Close when the backdrop is clicked. */
    lightDismiss?: boolean;
    /** Focus target after closing when the opener is gone. */
    invoker?: HTMLElement | null;
    /** Unmodified keys that still reach the application's shortcuts. */
    shortcuts?: readonly string[];
    children: Snippet;
  }
  let {
    open,
    onCancel,
    labelledby,
    describedby,
    role = "dialog",
    lightDismiss = false,
    invoker,
    shortcuts = [],
    children,
  }: Props = $props();
  let dialog: HTMLDialogElement;

  $effect(() => {
    if (!open) return;
    const opener =
      invoker ??
      (document.activeElement instanceof HTMLElement
        ? document.activeElement
        : null);
    dialog.showModal();
    return () => {
      dialog.close();
      void tick().then(() => {
        if (!hasFocus() && canFocus(opener))
          opener.focus({ preventScroll: true });
      });
    };
  });

  function keydown(event: KeyboardEvent) {
    const modified =
      event.metaKey || event.ctrlKey || event.altKey || event.shiftKey;
    if (modified || !shortcuts.includes(event.key.toLowerCase()))
      event.stopPropagation();
  }
</script>

<dialog
  bind:this={dialog}
  class="float"
  {role}
  aria-labelledby={labelledby}
  aria-describedby={describedby}
  onkeydown={keydown}
  oncancel={(event) => {
    event.preventDefault();
    onCancel();
  }}
  onclose={() => {
    if (open) onCancel();
  }}
  onclick={(event) => {
    if (lightDismiss && event.target === dialog) onCancel();
  }}
>
  {@render children()}
</dialog>

<style>
  dialog {
    width: min(var(--dialog-width, 360px), calc(100vw - 2 * var(--space-4)));
    max-height: var(--dialog-height, calc(100svh - 2 * var(--space-4)));
    overflow: hidden;
    overscroll-behavior: contain;
  }
  dialog[open] {
    display: flex;
    flex-direction: column;
  }
</style>
