// Anchored tooltips for jargon, controls and chart points; the words live in vocabulary.ts.
import { fromAction, type Attachment } from "svelte/attachments";
import { nextFrame } from "../presentation/motion.svelte";
import { claim, isWarm, release, warmUp } from "./intent";
const ACTIONABLE_SELECTOR =
  "button, a, label, summary, [role='switch'], [role='tab']";
let uid = 0;
// A hand at rest on the word opens its tip; jargon answers sooner, the next tip after a short rest, never at once.
const REST_MS = 300;
const TERM_REST_MS = 200;
const WARM_REST_MS = 60;
const CLOSE_MS = 100;
const SWEEP_PX_PER_MS = 0.2;
const LONG_PRESS_MS = 450;
// A finger that drifts further is scrolling, not pressing.
const SLOP_PX = 10;
const GAP_PX = 8;
const anchored =
  typeof CSS !== "undefined" && CSS.supports("position-area", "block-start");
/** A plain label or control; the getter updates the text in place, so an open tip stays open. */
export const tooltip = (text: () => string) => fromAction(tooltipAction, text);
/** " (key)" for a tip while the key acts and a keyboard is likely at hand; a finger has no Esc. */
export const keyHint = (key: string, acts = true) =>
  acts && matchMedia("(any-hover: hover)").matches ? ` (${key})` : "";
/** Jargon: a quiet mark, a shorter rest, and a click or tap opens it outright. */
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

/** Live measurement views keep one action per element instead of rebuilding attachment factories on each frame. */
export const termAction = (node: HTMLElement, text: string) =>
  tooltipAction(node, text, true);

export function tooltipAction(
  node: HTMLElement,
  initial: string,
  marked = false,
) {
  let active: ReturnType<typeof createTooltip> | null = null;
  function update(text: string) {
    if (!text) {
      active?.destroy();
      active = null;
    } else if (active) active.update(text);
    else active = createTooltip(node, text, marked);
  }
  update(initial);
  return {
    update,
    destroy() {
      active?.destroy();
      active = null;
    },
  };
}

function createTooltip(node: HTMLElement, initial: string, marked: boolean) {
  let text = initial;
  const id = `gm-tt-${++uid}`;
  let bubble: HTMLDivElement | null = null;
  let prevDescribedBy: string | null = null;
  let openTimer = 0;
  let closeTimer = 0;
  let pressTimer = 0;
  // Where a finger landed, until it lifts, drifts away or the page takes it to scroll.
  let press: { x: number; y: number } | null = null;
  // A long press showed the tip; the click it ends in is not a tap on the control.
  let pressed = false;
  // A touch showed the tip; the next tap closes it.
  let touchOpen = false;
  // A click answers the pointer; a control's tip waits until it leaves and comes back.
  let clicked = false;
  // Keyboard focus showed the tip; it stays with its word while the page scrolls.
  let keyboard = false;
  let lastPointer = "";
  const anchorNames = node.style.getPropertyValue("anchor-name");
  node.style.setProperty(
    "anchor-name",
    anchorNames ? `${anchorNames}, --${id}` : `--${id}`,
  );
  // A term inside a control explains while that control has keyboard focus.
  const host = node.closest<HTMLElement>(ACTIONABLE_SELECTOR) ?? node;
  const inert = host === node && !node.matches(ACTIONABLE_SELECTOR);
  // Jargon and explained facts answer a click or tap; a control does its job and explains on a pause or long press.
  const asks = inert || marked;
  if (!node.matches(ACTIONABLE_SELECTOR))
    node.dataset.tip = marked ? "term" : "";
  const focusable =
    inert && node.tabIndex < 0 && !node.hasAttribute("tabindex");
  if (focusable) node.tabIndex = node.closest("[data-tip-group]") ? -1 : 0;
  // A multi-line tip is an explainer: its first line titles the rest; a tab splits a line into an aligned pair.
  function write(target: HTMLElement) {
    target.toggleAttribute("data-titled", text.includes("\n"));
    if (!text.includes("\t")) return void (target.textContent = text);
    target.replaceChildren(
      ...text.split("\n").map((line) => {
        const row = document.createElement("div");
        const [label, value] = line.split("\t");
        if (value === undefined) row.textContent = line;
        else {
          row.className = "inspect-row";
          row.append(
            ...[label, value].map((part) =>
              Object.assign(document.createElement("span"), {
                textContent: part,
              }),
            ),
          );
        }
        return row;
      }),
    );
  }
  // Without anchor positioning the bubble sits above the word if it fits, else below, inside the viewport.
  function place(target: HTMLElement) {
    const at = node.getBoundingClientRect();
    const { offsetWidth: width, offsetHeight: height } = target;
    const above = at.top - height - GAP_PX >= GAP_PX;
    const left = at.left + at.width / 2 - width / 2;
    target.style.top = `${above ? at.top - height - GAP_PX : at.bottom + GAP_PX}px`;
    target.style.left = `${Math.max(GAP_PX, Math.min(left, innerWidth - width - GAP_PX))}px`;
  }
  // Which side of the term the bubble settled on.
  function aim(target: HTMLElement) {
    const at = node.getBoundingClientRect();
    const box = target.getBoundingClientRect();
    target.dataset.side = box.top >= at.top ? "below" : "above";
  }
  function show(touch = false, byKeyboard = false) {
    cancelOpen();
    clearTimeout(closeTimer);
    if (bubble || !text || !node.isConnected) return;
    keyboard = byKeyboard;
    // Moving between tips swaps them without the entry scale.
    const instant = isWarm();
    claim(hide);
    bubble = document.createElement("div");
    bubble.className = "tooltip";
    bubble.id = id;
    bubble.popover = "manual";
    bubble.setAttribute("role", "tooltip");
    bubble.style.setProperty("position-anchor", `--${id}`);
    write(bubble);
    if (instant) bubble.dataset.show = "true";
    document.body.appendChild(bubble);
    bubble.showPopover();
    if (!anchored) place(bubble);
    aim(bubble);
    prevDescribedBy = node.getAttribute("aria-describedby");
    node.setAttribute(
      "aria-describedby",
      prevDescribedBy ? `${prevDescribedBy} ${id}` : id,
    );
    // A tip a finger opened takes the tap that closes it, so the tap never lands on what lies beneath.
    if (touch) {
      touchOpen = true;
      bubble.style.pointerEvents = "auto";
      bubble.addEventListener("click", hide);
    }
    if (!instant) nextFrame(() => bubble?.setAttribute("data-show", "true"));
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
  // A pointer's tip closes as the page scrolls; a focused word keeps its tip beside it.
  function onScroll() {
    if (!keyboard) hide();
    else if (bubble && !anchored) {
      place(bubble);
      aim(bubble);
    }
  }
  function hide() {
    cancelOpen();
    clearTimeout(closeTimer);
    endPress();
    touchOpen = false;
    release(hide);
    if (!bubble) return;
    warmUp();
    const leaving = bubble;
    leaving.removeAttribute("role");
    leaving.setAttribute("aria-hidden", "true");
    leaving.style.pointerEvents = "none";
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
  // Mouse and pen hover; a finger never does.
  const hovers = (event: PointerEvent) => event.pointerType !== "touch";
  function onEnter(event: PointerEvent) {
    if (!hovers(event)) return;
    clearTimeout(closeTimer);
    sweeping(event);
    settle(event);
  }
  function cancelOpen() {
    clearTimeout(openTimer);
    openTimer = 0;
  }
  // The hand's last position and time: faster than SWEEP_PX_PER_MS between two moves is a sweep, not a reading
  // hand's drift.
  let last = { x: 0, y: 0, t: 0 };
  function sweeping(event: PointerEvent) {
    const dt = event.timeStamp - last.t;
    const fast =
      dt > 0 &&
      Math.hypot(event.clientX - last.x, event.clientY - last.y) / dt >
        SWEEP_PX_PER_MS;
    last = { x: event.clientX, y: event.clientY, t: event.timeStamp };
    return fast;
  }
  // Hover intent: a hand on the word for a moment opens the tip; a drag never counts, and a sweep leaves first.
  function settle(event: PointerEvent) {
    if (!hovers(event) || event.buttons || bubble || clicked || openTimer)
      return;
    // Not motion: hover intent is a moment on the word.
    openTimer = window.setTimeout(
      () => {
        openTimer = 0;
        show();
      },
      isWarm() ? WARM_REST_MS : marked ? TERM_REST_MS : REST_MS,
    );
  }
  function onLeave(event: PointerEvent) {
    if (!hovers(event)) return;
    cancelOpen();
    clicked = false;
    // Not motion: a slip off a small word keeps its tip a moment.
    if (bubble && !touchOpen) closeTimer = window.setTimeout(hide, CLOSE_MS);
  }
  // Keyboard focus only (main.ts drops data-pointer on a key), a microtask later, once a closing popover has restored focus.
  function onFocus(event: FocusEvent) {
    const target = event.target as HTMLElement;
    queueMicrotask(() => {
      if (
        target.matches(":focus-visible") &&
        !("pointer" in document.documentElement.dataset)
      )
        show(false, true);
    });
  }
  function endPress() {
    clearTimeout(pressTimer);
    press = null;
  }
  function onPointerDown(event: PointerEvent) {
    lastPointer = event.pointerType;
    pressed = false;
    if (event.pointerType === "touch") {
      press = { x: event.clientX, y: event.clientY };
      if (!touchOpen && !asks)
        // Not motion: a long press asks a control for its tip.
        pressTimer = window.setTimeout(() => {
          show(true);
          pressed = !!bubble;
        }, LONG_PRESS_MS);
      return;
    }
    cancelOpen();
    clicked = true;
    if (asks) show();
    else hide();
  }
  function onPointerMove(event: PointerEvent) {
    if (
      press &&
      Math.hypot(event.clientX - press.x, event.clientY - press.y) > SLOP_PX
    )
      endPress();
    // A sweeping hand has not rested: its moment starts over where it is now.
    if (sweeping(event) && openTimer) cancelOpen();
    settle(event);
  }
  // A tap on jargon or a fact toggles its tip; a tap on a control only runs it.
  function onPointerUp(event: PointerEvent) {
    if (event.pointerType !== "touch" || !press) return;
    endPress();
    if (pressed) return;
    if (touchOpen) hide();
    else if (asks) show(true);
  }
  function onClick(event: MouseEvent) {
    if (!pressed && !marked) return;
    pressed = false;
    event.preventDefault();
    event.stopPropagation();
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
    [document, "scroll", onScroll, true],
  ] as const;
  const listeners = [
    [node, "pointerdown", onPointerDown],
    [node, "pointerenter", onEnter],
    [node, "pointerleave", onLeave],
    [node, "pointermove", onPointerMove],
    [node, "pointerup", onPointerUp],
    [node, "pointercancel", endPress],
    [node, "contextmenu", onContextMenu],
    [node, "click", onClick],
    [host, "focusin", onFocus],
    [host, "focusout", hide],
  ] as const;
  for (const [target, type, listener] of listeners)
    target.addEventListener(type, listener as EventListener);
  return {
    update(next: string) {
      if (next === text) return;
      text = next;
      if (!bubble) return;
      if (!text) hide();
      else {
        write(bubble);
        if (!anchored) place(bubble);
        aim(bubble);
      }
    },
    destroy() {
      hide();
      if (anchorNames) node.style.setProperty("anchor-name", anchorNames);
      else node.style.removeProperty("anchor-name");
      delete node.dataset.tip;
      if (focusable) node.removeAttribute("tabindex");
      for (const [target, type, listener] of listeners)
        target.removeEventListener(type, listener as EventListener);
    },
  };
}
