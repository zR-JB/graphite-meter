// Tooltips by delegation, one bubble at a time: `ref={tip(() => text)}`. A hand at rest opens a tip, a sweep never
// does; jargon and facts answer a tap or click; a long press asks a control.
import { nextFrame, pageMs } from "../../lib/frame";
import { computed, effect, onCleanup, scope, signal, type Read } from "../../lib/signal";

type Kind = "tip" | "term" | "clip";
interface Entry {
  text: Read<string>;
  kind: Kind;
}

const ACTIONABLE = "button, a, label, summary, [role='switch'], [role='tab']";
const REST_PX = 8;
const REST_MS = 650;
const TERM_REST_MS = 450;
const WARM_REST_MS = 200;
const WARM_MS = 600;
const CLOSE_MS = 150;
const LONG_PRESS_MS = 450;
/** A finger that drifts further is scrolling, not pressing. */
const SLOP_PX = 10;
const GAP_PX = 8;

const entries = new WeakMap<Element, Entry>();
let ids = 0;

/** A plain tip for a control or label; the getter may change while it shows. */
export const tip =
  (text: () => string, kind: Kind = "tip") =>
  (node: HTMLElement) =>
    register(node, text, kind);

/** Jargon: a quiet mark, a shorter rest, and a click or tap opens it outright. */
export const term = (text: () => string) => tip(text, "term");

/** " (key)" while the key acts. */
export const keyHint = (key: string, acts = true) => (acts ? ` (${key})` : "");

function register(node: HTMLElement, text: () => string, kind: Kind): void {
  const entry = { text: computed(text), kind };
  entries.set(node, entry);
  const shown = computed(() => entry.text() !== "");
  const actionable = node.matches(ACTIONABLE);
  // Whether this gave the node its tab stop, so an emptied tip takes back only its own.
  let gave = false;
  effect(() => {
    // A word or figure says it explains (the help cursor, jargon's mark); a control explains on its own.
    if (!actionable && shown()) node.dataset.tip = kind === "term" ? "term" : "";
    else if (!actionable) delete node.dataset.tip;
    if (!shown() && open?.node === node) hide();
    if (actionable) return;
    // The keyboard reaches a stand-alone word that explains once it is placed: one stop per group of facts.
    queueMicrotask(() => {
      if (shown.peek() === gave) return;
      if (gave) node.removeAttribute("tabindex");
      else if (!node.closest(ACTIONABLE) && !node.hasAttribute("tabindex"))
        node.tabIndex = node.closest("[data-tip-group]") ? -1 : 0;
      else return;
      gave = !gave;
    });
  });
  onCleanup(() => {
    entries.delete(node);
    if (open?.node === node) hide();
  });
}

interface Open {
  node: HTMLElement;
  bubble: HTMLDivElement;
  pinned: boolean;
  /** Keyboard focus opened it: it stays beside its word while the page scrolls. */
  keyboard: boolean;
  describedBy: string | null;
  stop: () => void;
}

let open: Open | null = null;
let coolUntil = 0;
/** Where a mouse or pen came to rest, and the node it rests on. */
let rest: { node: HTMLElement; x: number; y: number; timer: number } | null = null;
let closing = 0;
/** A click answers the pointer: a control's tip waits until the pointer leaves it and comes back. */
let clicked: HTMLElement | null = null;
/** A finger or long-pressing mouse, until it lifts or drifts away. */
let press: { node: HTMLElement; x: number; y: number; timer: number; opened: boolean } | null = null;
let lastPointer = "";

let anchoring: boolean | undefined;
const anchored = () => (anchoring ??= CSS.supports("position-area", "block-start"));
const explainOnClick = () => document.documentElement.dataset.explanations === "click";
const warm = () => !!open || pageMs() < coolUntil;

/** A plot's reading just closed: the next tip nearby answers a short rest. */
export const warmTips = () => {
  coolUntil = pageMs() + WARM_MS;
};

/** The explained element at or above `target`; cut text yields to an element around it that explains itself. */
function find(target: EventTarget | null): [HTMLElement, Entry] | null {
  let cut: [HTMLElement, Entry] | null = null;
  for (let node = target instanceof Element ? target : null; node; node = node.parentElement) {
    const entry = entries.get(node);
    if (!entry?.text.peek()) continue;
    if (entry.kind !== "clip") return [node as HTMLElement, entry];
    cut ??= [node as HTMLElement, entry];
  }
  return cut;
}

/** Jargon and stand-alone facts answer a click or tap; a control does its job and explains on a rest. */
const traits = (node: HTMLElement, entry: Entry) => {
  const actionable = node.matches(ACTIONABLE);
  const inert = !actionable && !node.closest(ACTIONABLE);
  return { asks: inert || entry.kind === "term", explains: entry.kind !== "clip" && !actionable };
};

/** A multi-line tip is an explainer: its first line titles the rest; a tab splits a line into an aligned pair. */
function write(bubble: HTMLElement, text: string): void {
  bubble.toggleAttribute("data-titled", text.includes("\n"));
  if (!text.includes("\t")) {
    bubble.textContent = text;
    return;
  }
  bubble.replaceChildren(
    ...text.split("\n").map((line) => {
      const row = document.createElement("div");
      const [label, value] = line.split("\t");
      if (value === undefined) row.textContent = line;
      else {
        row.className = "inspect-row";
        for (const part of [label!, value])
          row.append(Object.assign(document.createElement("span"), { textContent: part }));
      }
      return row;
    }),
  );
}

/** Without anchor positioning the bubble sits above its word if it fits, else below, inside the viewport. */
function place({ node, bubble }: Open): void {
  const at = node.getBoundingClientRect();
  if (!anchored()) {
    const { offsetWidth: width, offsetHeight: height } = bubble;
    const above = at.top - height - GAP_PX >= GAP_PX;
    bubble.style.top = `${above ? at.top - height - GAP_PX : at.bottom + GAP_PX}px`;
    bubble.style.left = `${Math.max(GAP_PX, Math.min(at.left + at.width / 2 - width / 2, innerWidth - width - GAP_PX))}px`;
  }
  bubble.dataset.side = bubble.getBoundingClientRect().top >= at.top ? "below" : "above";
}

function show(node: HTMLElement, entry: Entry, pinned = false, keyboard = false): void {
  cancelRest();
  clearTimeout(closing);
  if (open?.node === node || !node.isConnected) return;
  const instant = warm();
  hide();
  const id = `gm-tt-${++ids}`;
  const bubble = document.createElement("div");
  bubble.className = "tooltip";
  bubble.id = id;
  bubble.popover = "manual";
  bubble.setAttribute("role", "tooltip");
  bubble.style.setProperty("position-anchor", `--${id}`);
  // One tip anchor at a time, on the element whose tip shows, beside any anchor of its own; a tip still fading from
  // this element gives way at once.
  const names = anchorNames(node);
  for (const name of names.filter((each) => TIP_ANCHOR.test(each))) document.getElementById(name.slice(2))?.remove();
  setAnchors(node, [...names.filter((each) => !TIP_ANCHOR.test(each)), `--${id}`]);
  if (instant) bubble.dataset.show = "true";
  const describedBy = node.getAttribute("aria-describedby");
  node.setAttribute("aria-describedby", describedBy ? `${describedBy} ${id}` : id);
  // A pinned tip takes the tap or click that closes it, so it never lands on what lies beneath.
  if (pinned) {
    bubble.style.pointerEvents = "auto";
    bubble.addEventListener("click", hide);
  }
  const current: Open = { node, bubble, pinned, keyboard, describedBy, stop: () => {} };
  open = current;
  document.body.append(bubble);
  [, current.stop] = scope(() =>
    effect(() => {
      const text = entry.text();
      if (!text) return queueMicrotask(hide);
      write(bubble, text);
      if (open === current && bubble.isConnected) place(current);
    }),
  );
  // A tip asked for inside another popover's show steps would throw there, so it shows in the next task.
  setTimeout(() => {
    if (open !== current || !bubble.isConnected) return;
    bubble.showPopover();
    place(current);
    if (!instant) nextFrame(() => bubble.setAttribute("data-show", "true"));
  });
  listen(true);
}

function hide(): void {
  cancelRest();
  clearTimeout(closing);
  const current = open;
  if (!current) return;
  open = null;
  warmTips();
  current.stop();
  const { node, bubble, describedBy } = current;
  if (describedBy === null) node.removeAttribute("aria-describedby");
  else node.setAttribute("aria-describedby", describedBy);
  bubble.removeAttribute("role");
  bubble.setAttribute("aria-hidden", "true");
  bubble.style.pointerEvents = "none";
  bubble.dataset.show = "false";
  // The bubble keeps its anchor while it fades, so it fades where it stood.
  void Promise.allSettled(bubble.getAnimations().map((animation) => animation.finished)).then(() => {
    bubble.remove();
    setAnchors(
      node,
      anchorNames(node).filter((name) => name !== `--${bubble.id}`),
    );
  });
  listen(false);
}

const TIP_ANCHOR = /^--gm-tt-\d+$/;
const anchorNames = (node: HTMLElement) =>
  node.style
    .getPropertyValue("anchor-name")
    .split(",")
    .map((name) => name.trim())
    .filter(Boolean);
function setAnchors(node: HTMLElement, names: readonly string[]): void {
  if (names.length) node.style.setProperty("anchor-name", names.join(", "));
  else node.style.removeProperty("anchor-name");
}

function cancelRest(): void {
  if (rest) clearTimeout(rest.timer);
  rest = null;
}

function endPress(): void {
  if (press) clearTimeout(press.timer);
  press = null;
}

// Mouse and pen hover; a finger never does. Decided per event: hover media queries misreport too often.
const hovers = (event: PointerEvent) => event.pointerType !== "touch";

/** Hover intent: only a hand that stays near one point opens the tip; a sweep keeps moving it, a drag never counts. */
function settle(event: PointerEvent): void {
  const found = find(event.target);
  if (!found || !hovers(event) || event.buttons) return;
  const [node, entry] = found;
  if (open?.node === node || clicked === node || (traits(node, entry).explains && explainOnClick())) return;
  const { clientX: x, clientY: y } = event;
  if (rest?.node === node && Math.hypot(x - rest.x, y - rest.y) <= REST_PX) return;
  cancelRest();
  const wait = warm() ? WARM_REST_MS : entry.kind === "term" ? TERM_REST_MS : REST_MS;
  // Not motion: hover intent is judged by rest time; a reading hand drifts within REST_PX.
  rest = { node, x, y, timer: window.setTimeout(() => show(node, entry), wait) };
}

function pointerover(event: PointerEvent): void {
  if (!hovers(event)) return;
  if (open && open.node.contains(event.target as Node)) clearTimeout(closing);
  settle(event);
}

function pointerout(event: PointerEvent): void {
  if (!hovers(event)) return;
  const found = find(event.target);
  if (!found || found[0].contains(event.relatedTarget as Node | null)) return;
  const [node] = found;
  if (rest?.node === node) cancelRest();
  if (clicked === node) clicked = null;
  // Not motion: a slip off a small word keeps its tip a moment.
  if (open?.node === node && !open.pinned) closing = window.setTimeout(hide, CLOSE_MS);
}

function pointerdown(event: PointerEvent): void {
  lastPointer = event.pointerType;
  const target = event.target as Node;
  if (open && !open.node.contains(target) && !open.bubble.contains(target)) hide();
  const found = find(event.target);
  if (!found) return;
  const [node, entry] = found;
  const { asks, explains } = traits(node, entry);
  if (hovers(event)) {
    cancelRest();
    clicked = node;
    const asked = explains && explainOnClick();
    if (asked && asks && open?.node === node) return hide();
    if (asks) return show(node, entry, asked);
    hide();
    if (!asked) return;
  }
  endPress();
  const current = { node, x: event.clientX, y: event.clientY, timer: 0, opened: false };
  press = current;
  if (!asks && open?.node !== node)
    // Not motion: a long press asks a control for its tip, with a mouse too once explanations wait to be asked.
    current.timer = window.setTimeout(() => {
      show(node, entry, true);
      current.opened = open?.node === node;
    }, LONG_PRESS_MS);
}

function pointermove(event: PointerEvent): void {
  if (press && Math.hypot(event.clientX - press.x, event.clientY - press.y) > SLOP_PX) endPress();
  settle(event);
}

/** A tap on jargon or a fact toggles its tip; a tap on a control only runs it. */
function pointerup(event: PointerEvent): void {
  const current = press;
  if (!current) return;
  clearTimeout(current.timer);
  if (current.opened || event.pointerType !== "touch") return;
  press = null;
  const entry = entries.get(current.node);
  if (open?.node === current.node && open.pinned) hide();
  else if (entry && traits(current.node, entry).asks) show(current.node, entry, true);
}

/** A click that ends a long press which showed a tip, and any click on jargon, never acts on the control. */
function click(event: MouseEvent): void {
  const found = find(event.target);
  const longPressed = press?.opened && press.node === found?.[0];
  if (longPressed) press = null;
  if (!found || (!longPressed && found[1].kind !== "term")) return;
  event.preventDefault();
  event.stopPropagation();
}

function contextmenu(event: Event): void {
  if (lastPointer === "touch" && find(event.target)) event.preventDefault();
}

/** Keyboard focus only (a key drops data-pointer), a microtask later, once a closing popover restored focus. */
function focusin(event: FocusEvent): void {
  const target = event.target as HTMLElement;
  queueMicrotask(() => {
    if (!target.matches(":focus-visible") || "pointer" in document.documentElement.dataset) return;
    const found = find(target) ?? findInside(target);
    if (found) show(found[0], found[1], false, true);
  });
}

/** A focused control explains through a term inside it. */
function findInside(host: HTMLElement): [HTMLElement, Entry] | null {
  if (!host.matches(ACTIONABLE)) return null;
  for (const node of host.querySelectorAll<HTMLElement>("[data-tip]")) {
    const entry = entries.get(node);
    if (entry?.text.peek()) return [node, entry];
  }
  return null;
}

function focusout(event: FocusEvent): void {
  if (open && (event.target as Node).contains(open.node)) hide();
}

const escape = (event: KeyboardEvent) => {
  if (event.key !== "Escape") return;
  // An open tip takes Escape, so it never reaches page shortcuts such as stopping a run.
  hide();
  event.preventDefault();
  event.stopPropagation();
};
const toggle = (event: Event) => {
  if ((event as ToggleEvent).newState === "closed" && open && (event.target as Node).contains(open.node)) hide();
};
const visibility = () => {
  if (document.visibilityState !== "visible") hide();
};
/** A pointer's tip closes as the page scrolls; a focused word keeps its tip beside it. */
const scroll = () => {
  if (open && !open.keyboard) hide();
  else if (open && !anchored()) place(open);
};

/** Global dismissals exist only while a tip is open. */
function listen(on: boolean): void {
  const method = on ? "addEventListener" : "removeEventListener";
  window[method]("blur", hide);
  document[method]("visibilitychange", visibility);
  document[method]("toggle", toggle, true);
  document[method]("keydown", escape as EventListener, true);
  document[method]("scroll", scroll, true);
}

/** Installs the page's listeners once. */
export function installTips(): void {
  const on = <K extends keyof DocumentEventMap>(type: K, listener: (event: DocumentEventMap[K]) => void) =>
    document.addEventListener(type, listener, true);
  on("pointerover", pointerover);
  on("pointerout", pointerout);
  on("pointermove", pointermove);
  on("pointerdown", pointerdown);
  on("pointerup", pointerup);
  on("pointercancel", endPress);
  on("click", click);
  on("contextmenu", contextmenu);
  on("focusin", focusin);
  on("focusout", focusout);
}

/** Text an ellipsis may cut: while cut, its whole text (or `full`) is its tip. */
export function clip(full?: () => string): (node: HTMLElement) => void {
  return (node) => {
    // Bumped whenever the box or its text changes, so the tip's text follows both.
    const seen = signal(0);
    register(
      node,
      () => {
        seen();
        if (node.scrollWidth <= node.clientWidth + 1) return "";
        return (full?.() ?? node.textContent ?? "").replace(/\s+/g, " ").trim();
      },
      "clip",
    );
    clipped.set(node, () => seen.set(seen.peek() + 1));
    resized.observe(node);
    edited.observe(node, EDITS);
    onCleanup(() => {
      clipped.delete(node);
      resized.unobserve(node);
      rewatch();
    });
  };
}

const EDITS: MutationObserverInit = { childList: true, characterData: true, subtree: true };
const clipped = new Map<Element, () => void>();
const recheck = (node: Element | null) => node && clipped.get(node)?.();
const resized = new ResizeObserver((records) => {
  for (const record of records) recheck(record.target);
});
const edits = (records: MutationRecord[]) => {
  for (const record of records)
    for (let node: Node | null = record.target; node; node = node.parentNode)
      if (node instanceof Element && clipped.has(node)) recheck(node);
};
const edited = new MutationObserver(edits);
let rewatching = false;
/** A mutation observer cannot let go of one node: once a task's removals are in, it watches the rest afresh. */
function rewatch(): void {
  if (rewatching) return;
  rewatching = true;
  queueMicrotask(() => {
    rewatching = false;
    const pending = edited.takeRecords();
    edited.disconnect();
    for (const node of clipped.keys()) edited.observe(node, EDITS);
    edits(pending);
  });
}
