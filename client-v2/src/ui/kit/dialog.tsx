// Native dialogs: modal ones trap focus and carry their own live region; the owner keeps the open state and hears
// every close request once. Focus returns to the opener unless the user already moved it.
import type { Child } from "../../lib/jsx-runtime";
import { effect, onCleanup, untrack, type Read } from "../../lib/signal";
import { Announcements } from "./announce";
import { canFocus, hasFocus } from "./focus";

export interface DialogProps {
  open: Read<boolean>;
  modal?: boolean;
  onCancel: () => void;
  /** A click on the backdrop closes it. */
  lightDismiss?: boolean;
  /** Where focus returns if the opener is gone; the opener is the focused element when it opens. */
  invoker?: () => HTMLElement | null;
  class?: string;
  role?: "dialog" | "alertdialog";
  label?: string;
  labelledby?: string;
  describedby?: string;
  children: Child;
}

export function Dialog(props: DialogProps): HTMLDialogElement {
  const modal = props.modal ?? true;
  // A close can follow a cancel the owner is still applying: report once.
  let reported = false;
  const cancel = () => {
    if (reported) return;
    reported = true;
    props.onCancel();
  };
  const dialog = (
    <dialog
      class={props.class ?? "float"}
      role={props.role ?? "dialog"}
      aria-label={props.label}
      aria-labelledby={props.labelledby}
      aria-describedby={props.describedby}
      inert={() => !props.open()}
      oncancel={(event) => {
        event.preventDefault();
        cancel();
      }}
      onclose={(event) => {
        if (props.open.peek() && !event.currentTarget.open) cancel();
      }}
      onclick={(event) => {
        if (!props.lightDismiss || !modal || event.target !== event.currentTarget) return;
        const box = event.currentTarget.getBoundingClientRect();
        const inside =
          event.clientX >= box.left &&
          event.clientX <= box.right &&
          event.clientY >= box.top &&
          event.clientY <= box.bottom;
        if (!inside) cancel();
      }}
    >
      {props.children}
      {modal && <Announcements />}
    </dialog>
  ) as HTMLDialogElement;
  effect(() => {
    if (!props.open()) return;
    untrack(() => {
      reported = false;
      const opener = props.invoker?.() ?? document.activeElement;
      // A dialog built open is shown once it is in the page.
      const show = () => {
        if (!props.open.peek() || dialog.open) return;
        if (modal) dialog.showModal();
        else dialog.show();
      };
      if (dialog.isConnected) show();
      else queueMicrotask(show);
      onCleanup(() => {
        dialog.close();
        queueMicrotask(() => {
          if (!hasFocus() && canFocus(opener)) opener.focus({ preventScroll: true });
        });
      });
    });
  });
  return dialog;
}

/** A question before something that cannot be undone; Cancel comes first and takes focus. */
export function ConfirmDialog(props: {
  open: Read<boolean>;
  id: string;
  title: Child;
  description: Child;
  confirm: Child;
  cancel?: Child;
  invoker?: () => HTMLElement | null;
  onConfirm: () => void;
  onCancel: () => void;
}): HTMLDialogElement {
  return (
    <Dialog
      open={props.open}
      invoker={props.invoker}
      onCancel={props.onCancel}
      role="alertdialog"
      labelledby={`${props.id}-title`}
      describedby={`${props.id}-description`}
    >
      <div class="confirm">
        <h2 id={`${props.id}-title`}>{props.title}</h2>
        <p id={`${props.id}-description`}>{props.description}</p>
        <div class="confirm-actions">
          <button class="btn btn-inset" type="button" onclick={props.onCancel}>
            {props.cancel ?? "Cancel"}
          </button>
          <button class="btn btn-danger btn-solid" type="button" onclick={props.onConfirm}>
            {props.confirm}
          </button>
        </div>
      </div>
    </Dialog>
  ) as HTMLDialogElement;
}
