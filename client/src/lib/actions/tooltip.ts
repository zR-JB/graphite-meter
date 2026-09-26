// Svelte tooltip action plus the shared jargon dictionary for metric labels and settings controls.
const ACTIONABLE_SELECTOR = "button, a, label, [role='switch'], [role='tab']";
interface TooltipOptions {
  text: string;
  // Chart/plot tooltips track the pointer immediately; normal UI tips wait.
  instant?: boolean;
}
type TooltipParam = string | TooltipOptions;
let uid = 0;
const HOVER_DELAY_MS = 350;
const TOUCH_DISMISS_MS = 4000;
function normalize(param: TooltipParam): TooltipOptions {
  return typeof param === "string" ? { text: param } : param;
}
export function tooltip(node: HTMLElement, param: TooltipParam) {
  let opts = normalize(param);
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
  // Definitions and notes need a tab stop; other anchors already name their text.
  if (
    !node.hasAttribute("tabindex") &&
    node.tabIndex < 0 &&
    node.matches(".term, [role='note']")
  ) {
    node.tabIndex = 0;
  }
  function show() {
    if (bubble || !opts.text || !node.isConnected) return;
    bubble = document.createElement("div");
    bubble.className = "tooltip";
    bubble.id = id;
    bubble.popover = "manual";
    bubble.setAttribute("role", "tooltip");
    bubble.style.setProperty("position-anchor", `--${id}`);
    bubble.textContent = opts.text;
    document.body.appendChild(bubble);
    bubble.showPopover();
    prevDescribedBy = node.getAttribute("aria-describedby");
    node.setAttribute(
      "aria-describedby",
      prevDescribedBy ? `${prevDescribedBy} ${id}` : id,
    );
    requestAnimationFrame(() => bubble?.setAttribute("data-show", "true"));
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
    if (opts.instant) {
      show();
      return;
    }
    clearHoverTimer();
    hoverTimer = window.setTimeout(() => {
      hoverTimer = 0;
      show();
    }, HOVER_DELAY_MS);
  }
  function onPointerLeave(event: PointerEvent) {
    if (event.pointerType !== "mouse") return;
    hide();
  }
  // Keyboard focus asks for the tip; focus landing from a click does not.
  function onFocus(event: FocusEvent) {
    if ((event.target as HTMLElement).matches(":focus-visible")) show();
  }
  // A tap on a control runs the control, so only inert jargon shows a tip on touch.
  function onPointerUp(event: PointerEvent) {
    if (event.pointerType !== "touch") return;
    if (touchOpen) {
      hide();
      return;
    }
    if (node.closest(ACTIONABLE_SELECTOR)) return;
    show();
    if (!bubble) return;
    touchOpen = true;
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
    update(next: TooltipParam) {
      opts = normalize(next);
      if (!bubble) return;
      if (!opts.text) hide();
      else bubble.textContent = opts.text;
    },
    destroy() {
      hide();
      for (const [type, listener] of nodeListeners)
        node.removeEventListener(type, listener as EventListener);
    },
  };
}
export const JARGON = {
  addedLatency:
    "Added latency: loaded median RTT minus idle median RTT for the same server. Negative values are kept. The grade labels the worst stage: A ≤5 ms, B ≤30, C ≤60, D ≤200, otherwise F.",
  jitter:
    "RTT variation: average absolute change between consecutive successful replies in one segment. Lower is steadier; timeouts are excluded.",
  wireRate:
    "Estimated physical-link rate, including forward-path protocol overhead.",
  latency:
    "Median round-trip time (RTT) to the server and back. Lower is faster.",
  unitBits: "Bits per second (Mbit/s, Gbit/s), used by internet plans.",
  unitBytes: "MB/s or GB/s, used by download managers. One byte is eight bits.",
  unitDecimal: "Decimal prefixes: 1,000 per step (kbit/s, Mbit/s, Gbit/s).",
  unitBinary: "Binary prefixes: 1,024 per step (Kibit/s, Mibit/s, Gibit/s).",
} as const;
