<script lang="ts">
  // Native modal <dialog>; the owner keeps open state and hears every close.
  import { tick, untrack, type Snippet } from "svelte";
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
  // A close can follow a cancel the owner is still applying: report once.
  let reported = false;
  function cancel() {
    if (reported) return;
    reported = true;
    onCancel();
  }

  $effect(() => {
    if (!open) return;
    reported = false;
    const opener = untrack(() => invoker) ?? document.activeElement;
    dialog.showModal();
    return () => {
      dialog.close();
      void tick().then(() => {
        if (!hasFocus() && opener instanceof HTMLElement && canFocus(opener))
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
    cancel();
  }}
  onclose={() => {
    if (open) cancel();
  }}
  onclick={(event) => {
    if (lightDismiss && event.target === dialog) cancel();
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
