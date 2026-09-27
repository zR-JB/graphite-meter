// Anchored tooltips for jargon, controls and chart points; the words live in vocabulary.ts.
import { fromAction, type Attachment } from "svelte/attachments";
import { nextFrame } from "../presentation/motion.svelte";
import { restDetector, warmUp } from "./intent";
const ACTIONABLE_SELECTOR =
  "button, a, label, summary, [role='switch'], [role='tab']";
let uid = 0;
const HOVER_DELAY_MS = 500;
// An explainer waits longer than a control's name, so it appears only when the pointer means to ask.
const EXPLAINER_DELAY_MS = 1000;
const LONG_PRESS_MS = 500;
const TOUCH_DISMISS_MS = 4000;
/** An attachment; the getter updates the text in place, so an open tip stays open. */
export const tooltip = (text: () => string) => fromAction(tooltipAction, text);

const STEP: Record<string, number> = {
  ArrowDown: 1,
  ArrowRight: 1,
  ArrowUp: -1,
  ArrowLeft: -1,
};

/** One tab stop for a set of explained facts (marked data-tip-group); arrows walk them and show each tip. */
export const tipGroup: Attachment<HTMLElement> = (node) => {
  const facts = () => [
    ...node.querySelectorAll<HTMLElement>("[data-tip][tabindex]"),
  ];
  const settle = () => {
    const list = facts();
    if (list.length && !list.some((fact) => fact.tabIndex === 0))
      list[0].tabIndex = 0;
  };
  function onKeydown(event: KeyboardEvent) {
    const list = facts();
    const at = list.indexOf(event.target as HTMLElement);
    if (at < 0) return;
    const to =
      event.key === "Home"
        ? 0
        : event.key === "End"
          ? list.length - 1
          : event.key in STEP
            ? (at + STEP[event.key] + list.length) % list.length
            : -1;
    if (to < 0) return;
    event.preventDefault();
    list[at].tabIndex = -1;
    list[to].tabIndex = 0;
    list[to].focus();
  }
  const observer = new MutationObserver(settle);
  observer.observe(node, { childList: true, subtree: true });
  queueMicrotask(settle);
  node.addEventListener("keydown", onKeydown);
  return () => {
    observer.disconnect();
    node.removeEventListener("keydown", onKeydown);
  };
};

function tooltipAction(node: HTMLElement, initial: string) {
  let text = initial;
  const id = `gm-tt-${++uid}`;
  let bubble: HTMLDivElement | null = null;
  let prevDescribedBy: string | null = null;
  let touchOpen = false;
  // A long press showed the tip; the click it ends in is not a tap on the control.
  let pressed = false;
  let autoDismissTimer = 0;
  const rest = restDetector(() => {
    show();
    if (!bubble || lastPointer !== "touch") return;
    touchOpen = pressed = true;
    // Not motion: a touch tip closes itself.
    autoDismissTimer = window.setTimeout(hide, TOUCH_DISMISS_MS);
  });
  let lastPointer = "";
  // A click answers the pointer; its tip waits until it leaves and comes back.
  let clicked = false;
  const anchorNames = node.style.getPropertyValue("anchor-name");
  node.style.setProperty(
    "anchor-name",
    anchorNames ? `${anchorNames}, --${id}` : `--${id}`,
  );
  // A term brightens on hover; one outside a control also takes a tab stop, shared within a tip group.
  const inert = !node.closest(ACTIONABLE_SELECTOR);
  if (!node.matches(ACTIONABLE_SELECTOR)) node.dataset.tip = "";
  if (inert && node.tabIndex < 0 && !node.hasAttribute("tabindex"))
    node.tabIndex = node.closest("[data-tip-group]") ? -1 : 0;
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
  function onDocumentPointerDown(event: PointerEvent) {
    const target = event.target as Node | null;
    if (target && (node.contains(target) || bubble?.contains(target))) return;
    hide();
  }
  function onVisibilityDismiss() {
    if (document.visibilityState !== "visible") hide();
  }
  function hide() {
    rest.cancel();
    if (autoDismissTimer) {
      clearTimeout(autoDismissTimer);
      autoDismissTimer = 0;
    }
    if (!bubble) return;
    warmUp();
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
  // An open tip takes Escape, so it never reaches page shortcuts such as stopping a run.
  function onEscape(event: KeyboardEvent) {
    if (event.key !== "Escape") return;
    hide();
    event.preventDefault();
    event.stopPropagation();
  }
  function onPointerMove(event: PointerEvent) {
    lastPointer = event.pointerType;
    if (event.pointerType === "mouse" && !bubble && !clicked)
      rest.move(
        event,
        text.includes("\n") ? EXPLAINER_DELAY_MS : HOVER_DELAY_MS,
      );
  }
  function onPointerLeave(event: PointerEvent) {
    if (event.pointerType !== "mouse") return;
    clicked = false;
    hide();
  }
  // Only :focus-visible shows the tip, a microtask later, once a closing popover has restored focus.
  function onFocus(event: FocusEvent) {
    const target = event.target as HTMLElement;
    queueMicrotask(() => {
      if (target.matches(":focus-visible")) show();
    });
  }
  // A tap on a control runs the control, so only an inert anchor shows a tip on tap; a long press shows any.
  function onPointerUp(event: PointerEvent) {
    if (event.pointerType !== "touch") return;
    rest.cancel();
    if (pressed) return;
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
    lastPointer = event.pointerType;
    pressed = false;
    if (event.pointerType === "touch") {
      if (!touchOpen) rest.move(event, LONG_PRESS_MS);
      return;
    }
    // A click on a term asks outright; a click on a control answers the pointer instead.
    const open = !bubble && inert;
    clicked = true;
    hide();
    if (open) show();
  }
  function onClick(event: MouseEvent) {
    if (pressed) {
      pressed = false;
      event.preventDefault();
      event.stopPropagation();
    } else if (!touchOpen) hide();
  }
  function onContextMenu(event: Event) {
    if (lastPointer === "touch") event.preventDefault();
  }
  const dismissListeners = [
    [window, "blur", hide, false],
    [document, "visibilitychange", onVisibilityDismiss, false],
    [document, "toggle", onPopoverToggle, true],
    [document, "pointerdown", onDocumentPointerDown, true],
    [document, "keydown", onEscape, true],
  ] as const;
  const nodeListeners = [
    ["pointerdown", onPointerDown],
    ["pointerenter", onPointerMove],
    ["pointermove", onPointerMove],
    ["pointerleave", onPointerLeave],
    ["pointercancel", rest.cancel],
    ["contextmenu", onContextMenu],
    ["focusin", onFocus],
    ["focusout", hide],
    ["pointerup", onPointerUp],
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
