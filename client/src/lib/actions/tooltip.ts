// Anchored tooltips for jargon, controls and chart points; the words live in vocabulary.ts.
import { fromAction } from "svelte/attachments";
import { nextFrame } from "../presentation/motion.svelte";
const ACTIONABLE_SELECTOR =
  "button, a, label, summary, [role='switch'], [role='tab']";
let uid = 0;
const HOVER_DELAY_MS = 350;
const TOUCH_DISMISS_MS = 4000;
/** An attachment; the getter updates the text in place, so an open tip stays open. */
export const tooltip = (text: () => string) => fromAction(tooltipAction, text);

function tooltipAction(node: HTMLElement, initial: string) {
  let text = initial;
  const id = `gm-tt-${++uid}`;
  let bubble: HTMLDivElement | null = null;
  let prevDescribedBy: string | null = null;
  let touchOpen = false;
  let autoDismissTimer = 0;
  let hoverTimer = 0;
  const anchorNames = node.style.getPropertyValue("anchor-name");
  node.style.setProperty(
    "anchor-name",
    anchorNames ? `${anchorNames}, --${id}` : `--${id}`,
  );
  // A hint inside a control rides its focus and taps; any other anchor takes a tab stop.
  const inert = !node.closest(ACTIONABLE_SELECTOR);
  if (inert && node.tabIndex < 0 && !node.hasAttribute("tabindex"))
    node.tabIndex = 0;
  // A multi-line tip is an explainer: its first line titles the rest.
  function write(target: HTMLElement) {
    target.textContent = text;
    target.toggleAttribute("data-titled", text.includes("\n"));
  }
  function show() {
    if (bubble || !text || !node.isConnected) return;
    bubble = document.createElement("div");
    bubble.className = "tooltip";
    bubble.id = id;
    bubble.popover = "manual";
    bubble.setAttribute("role", "tooltip");
    bubble.style.setProperty("position-anchor", `--${id}`);
    write(bubble);
    document.body.appendChild(bubble);
    bubble.showPopover();
    prevDescribedBy = node.getAttribute("aria-describedby");
    node.setAttribute(
      "aria-describedby",
      prevDescribedBy ? `${prevDescribedBy} ${id}` : id,
    );
    nextFrame(() => bubble?.setAttribute("data-show", "true"));
    for (const [target, type, listener, capture] of dismissListeners)
      target.addEventListener(type, listener as EventListener, capture);
  }
  function onPopoverToggle(event: Event) {
    const host = event.target as Node;
    if ((event as ToggleEvent).newState === "closed" && host.contains(node))
      hide();
  }
  function clearHoverTimer() {
    if (hoverTimer) {
      clearTimeout(hoverTimer);
      hoverTimer = 0;
    }
  }
  function onDocumentPointerDown(event: PointerEvent) {
    const target = event.target as Node | null;
    if (target && (node.contains(target) || bubble?.contains(target))) return;
    hide();
  }
  function onVisibilityDismiss() {
    if (document.visibilityState !== "visible") hide();
  }
  function hide() {
    clearHoverTimer();
    if (autoDismissTimer) {
      clearTimeout(autoDismissTimer);
      autoDismissTimer = 0;
    }
    if (!bubble) return;
    const leaving = bubble;
    leaving.removeAttribute("role");
    leaving.setAttribute("aria-hidden", "true");
    leaving.dataset.show = "false";
    void Promise.allSettled(
      leaving.getAnimations().map((animation) => animation.finished),
    ).then(() => leaving.remove());
    bubble = null;
    if (prevDescribedBy === null) node.removeAttribute("aria-describedby");
    else node.setAttribute("aria-describedby", prevDescribedBy);
    prevDescribedBy = null;
    touchOpen = false;
    for (const [target, type, listener, capture] of dismissListeners)
      target.removeEventListener(type, listener as EventListener, capture);
  }
  function onKeydown(event: KeyboardEvent) {
    if (event.key === "Escape" && bubble) hide();
  }
  function onPointerEnter(event: PointerEvent) {
    if (event.pointerType !== "mouse") return;
    clearHoverTimer();
    // Not motion: a hovered tip waits before it opens.
    hoverTimer = window.setTimeout(() => {
      hoverTimer = 0;
      show();
    }, HOVER_DELAY_MS);
  }
  function onPointerLeave(event: PointerEvent) {
    if (event.pointerType !== "mouse") return;
    hide();
  }
  // Only :focus-visible shows the tip, a microtask later, once a closing popover has restored focus.
  function onFocus(event: FocusEvent) {
    const target = event.target as HTMLElement;
    queueMicrotask(() => {
      if (target.matches(":focus-visible")) show();
    });
  }
  // A tap on a control runs the control, so only an inert anchor shows a tip on touch.
  function onPointerUp(event: PointerEvent) {
    if (event.pointerType !== "touch") return;
    if (touchOpen) {
      hide();
      return;
    }
    if (!inert) return;
    show();
    if (!bubble) return;
    touchOpen = true;
    // Not motion: a touch tip closes itself.
    autoDismissTimer = window.setTimeout(hide, TOUCH_DISMISS_MS);
  }
  function onPointerDown(event: PointerEvent) {
    if (event.pointerType !== "touch") hide();
  }
  function onClick() {
    if (!touchOpen) hide();
  }
  const dismissListeners = [
    [window, "blur", hide, false],
    [document, "visibilitychange", onVisibilityDismiss, false],
    [document, "toggle", onPopoverToggle, true],
    [document, "pointerdown", onDocumentPointerDown, true],
  ] as const;
  const nodeListeners = [
    ["pointerdown", onPointerDown],
    ["pointerenter", onPointerEnter],
    ["pointerleave", onPointerLeave],
    ["focusin", onFocus],
    ["focusout", hide],
    ["pointerup", onPointerUp],
    ["keydown", onKeydown],
    ["click", onClick],
  ] as const;
  for (const [type, listener] of nodeListeners)
    node.addEventListener(type, listener as EventListener);
  return {
    update(next: string) {
      text = next;
      if (!bubble) return;
      if (!text) hide();
      else write(bubble);
    },
    destroy() {
      hide();
      for (const [type, listener] of nodeListeners)
        node.removeEventListener(type, listener as EventListener);
    },
  };
}
