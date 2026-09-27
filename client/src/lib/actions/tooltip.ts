// Anchored tooltips for jargon, controls and chart points; the words live in vocabulary.ts.
import { fromAction, type Attachment } from "svelte/attachments";
import { nextFrame } from "../presentation/motion.svelte";
import { restDetector, warmUp } from "./intent";
const ACTIONABLE_SELECTOR =
  "button, a, label, summary, [role='switch'], [role='tab']";
let uid = 0;
const TERM_REST_MS = 300;
const REST_MS = 600;
const LONG_PRESS_MS = 500;
/** A plain label or control; the getter updates the text in place, so an open tip stays open. */
export const tooltip = (text: () => string) => fromAction(tooltipAction, text);
/** Jargon: a quiet mark, a short rest, and a click or tap opens it outright. */
export const term = (text: () => string) =>
  fromAction(
    (node: HTMLElement, initial: string) => tooltipAction(node, initial, true),
    text,
  );

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

function tooltipAction(node: HTMLElement, initial: string, marked = false) {
  let text = initial;
  const id = `gm-tt-${++uid}`;
  let bubble: HTMLDivElement | null = null;
  let prevDescribedBy: string | null = null;
  let touchOpen = false;
  // A long press showed the tip; the click it ends in is not a tap on the control.
  let pressed = false;
  const rest = restDetector(() => {
    show();
    if (bubble && lastPointer === "touch") touchOpen = pressed = true;
  });
  let lastPointer = "";
  // A click answers the pointer; its tip waits until it leaves and comes back.
  let clicked = false;
  const anchorNames = node.style.getPropertyValue("anchor-name");
  node.style.setProperty(
    "anchor-name",
    anchorNames ? `${anchorNames}, --${id}` : `--${id}`,
  );
  // A term inside a control explains while that control has keyboard focus.
  const host = node.closest<HTMLElement>(ACTIONABLE_SELECTOR) ?? node;
  const inert = host === node && !node.matches(ACTIONABLE_SELECTOR);
  if (!node.matches(ACTIONABLE_SELECTOR))
    node.dataset.tip = marked ? "term" : "";
  if (inert && node.tabIndex < 0 && !node.hasAttribute("tabindex"))
    node.tabIndex = node.closest("[data-tip-group]") ? -1 : 0;
  // A multi-line tip is an explainer: its first line titles the rest.
  function write(target: HTMLElement) {
    target.textContent = text;
    target.toggleAttribute("data-titled", text.includes("\n"));
  }
  // The arrow points at the term wherever the bubble had to sit; measured about the centre, so the entry scale cancels.
  function aim(target: HTMLElement) {
    target.style.removeProperty("--arrow-x");
    const at = node.getBoundingClientRect();
    const box = target.getBoundingClientRect();
    const width = target.offsetWidth;
    target.dataset.side = box.top >= at.top ? "below" : "above";
    const x = at.left + at.width / 2 - (box.left + box.width / 2 - width / 2);
    target.style.setProperty(
      "--arrow-x",
      `${Math.min(Math.max(x, 12), width - 12)}px`,
    );
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
    aim(bubble);
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
    const popover = event.target as Node;
    if ((event as ToggleEvent).newState === "closed" && popover.contains(node))
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
    touchOpen = false;
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
      rest.move(event, marked ? TERM_REST_MS : REST_MS);
  }
  function onPointerLeave(event: PointerEvent) {
    if (event.pointerType !== "mouse") return;
    clicked = false;
    hide();
  }
  // Keyboard focus only (main.ts drops data-pointer on a key), a microtask later, once a closing popover has restored focus.
  function onFocus(event: FocusEvent) {
    const target = event.target as HTMLElement;
    queueMicrotask(() => {
      if (
        target.matches(":focus-visible") &&
        !("pointer" in document.documentElement.dataset)
      )
        show();
    });
  }
  // A tap on a marked term asks (never pressing its label); anything else needs a long press, so a tap still runs its control.
  function onPointerUp(event: PointerEvent) {
    if (event.pointerType !== "touch") return;
    rest.cancel();
    if (pressed) return;
    if (touchOpen) hide();
    else if (marked) {
      show();
      touchOpen = !!bubble;
    }
  }
  function onPointerDown(event: PointerEvent) {
    lastPointer = event.pointerType;
    pressed = false;
    if (event.pointerType === "touch") {
      if (!touchOpen) rest.move(event, LONG_PRESS_MS);
      return;
    }
    // A click on a term asks outright; a click on a control answers the pointer instead.
    const open = !bubble && (inert || marked);
    clicked = true;
    hide();
    if (open) show();
  }
  function onClick(event: MouseEvent) {
    if (pressed || marked) {
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
  const listeners = [
    [node, "pointerdown", onPointerDown],
    [node, "pointerenter", onPointerMove],
    [node, "pointermove", onPointerMove],
    [node, "pointerleave", onPointerLeave],
    [node, "pointercancel", rest.cancel],
    [node, "contextmenu", onContextMenu],
    [host, "focusin", onFocus],
    [host, "focusout", hide],
    [node, "pointerup", onPointerUp],
    [node, "click", onClick],
  ] as const;
  for (const [target, type, listener] of listeners)
    target.addEventListener(type, listener as EventListener);
  return {
    update(next: string) {
      text = next;
      if (!bubble) return;
      if (!text) hide();
      else {
        write(bubble);
        aim(bubble);
      }
    },
    destroy() {
      hide();
      for (const [target, type, listener] of listeners)
        target.removeEventListener(type, listener as EventListener);
    },
  };
}
