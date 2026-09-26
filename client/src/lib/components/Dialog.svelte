<script lang="ts">
  // Native <dialog>, modal or in flow; the owner keeps open state and hears every close.
  import { tick, untrack, type Snippet } from "svelte";
  import type { Attachment } from "svelte/attachments";
  import { canFocus, hasFocus } from "../actions/focus";

  // Explicit props and a template class: a spread or bare class expression pulls in clsx.
  interface Props {
    open: boolean;
    modal?: boolean;
    onCancel: () => void;
    /** Close when the backdrop is clicked. */
    lightDismiss?: boolean;
    /** Focus target after closing when the opener is gone. */
    invoker?: HTMLElement | null;
    class?: string;
    role?: "dialog" | "alertdialog";
    label?: string;
    labelledby?: string;
    describedby?: string;
    attach?: Attachment<HTMLElement>;
    children: Snippet;
  }
  let {
    open,
    modal = true,
    onCancel,
    lightDismiss = false,
    invoker,
    class: className = "float",
    role = "dialog",
    label,
    labelledby,
    describedby,
    attach,
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
    if (modal) dialog.showModal();
    else {
      dialog.show();
      if (opener instanceof HTMLElement) opener.focus({ preventScroll: true });
    }
    return () => {
      dialog.close();
      void tick().then(() => {
        if (!hasFocus() && opener instanceof HTMLElement && canFocus(opener))
          opener.focus({ preventScroll: true });
      });
    };
  });

  function backdropClick(event: MouseEvent) {
    if (!lightDismiss || !modal || event.target !== dialog) return;
    const box = dialog.getBoundingClientRect();
    const inside =
      event.clientX >= box.left &&
      event.clientX <= box.right &&
      event.clientY >= box.top &&
      event.clientY <= box.bottom;
    if (!inside) cancel();
  }
</script>

<dialog
  bind:this={dialog}
  class={`${className}`}
  {role}
  aria-label={label}
  aria-labelledby={labelledby}
  aria-describedby={describedby}
  inert={!open}
  {@attach attach}
  oncancel={(event) => {
    event.preventDefault();
    cancel();
  }}
  onclose={(event) => {
    if (open && !event.currentTarget.open) cancel();
  }}
  onclick={backdropClick}
>
  {@render children()}
</dialog>

<style>
  dialog.float {
    width: min(var(--dialog-width, 360px), calc(100vw - 2 * var(--space-4)));
    max-height: var(--dialog-height, calc(100svh - 2 * var(--space-4)));
    overflow: hidden;
    overscroll-behavior: contain;
  }
  dialog.float[open] {
    display: flex;
    flex-direction: column;
  }
</style>
