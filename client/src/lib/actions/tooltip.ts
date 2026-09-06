// Svelte tooltip action plus the shared jargon dictionary for metric labels and settings controls.
const ACTIONABLE_SELECTOR = "button, a, label, [role='switch'], [role='tab']";
interface TooltipOptions {
  text: string;
  placement?: "top" | "bottom";
  disabled?: boolean;
  // Chart/plot tooltips track the pointer immediately; normal UI tips wait.
  instant?: boolean;
}
type TooltipParam = string | TooltipOptions;
let uid = 0;
const STYLE_ID = "gm-tooltip-styles";
const HOVER_DELAY_MS = 350;
const TOUCH_DISMISS_MS = 4000;
function ensureStyles() {
  if (typeof document === "undefined") return;
  if (document.getElementById(STYLE_ID)) return;
  const style = document.createElement("style");
  style.id = STYLE_ID;
  style.textContent = `
    .gm-tooltip {
      position: fixed;
      z-index: 200;
      max-width: min(300px, calc(100vw - 16px));
      padding: var(--space-2) var(--space-3);
      border: 1px solid var(--border-strong);
      border-radius: var(--r-chrome);
      background: var(--surface-2);
      color: var(--text);
      box-shadow: 0 4px 12px rgba(var(--shadow-ink), 0.18);
      font-family: var(--font-sans);
      font-size: var(--type-sm);
      line-height: 1.4;
      font-weight: 500;
      letter-spacing: 0;
      text-transform: none;
      white-space: pre-line;
      overflow-wrap: anywhere;
      pointer-events: none;
      opacity: 0;
      transform: translateY(2px);
    }
    @media (prefers-reduced-motion: no-preference) {
      .gm-tooltip {
        transition:
          opacity var(--dur-hover) var(--ease-out),
          transform var(--dur-hover) var(--ease-out);
      }
    }
    .gm-tooltip[data-show="true"] {
      opacity: 1;
      transform: translateY(0);
    }
  `;
  document.head.appendChild(style);
}
function normalize(param: TooltipParam): TooltipOptions {
  return typeof param === "string" ? { text: param } : param;
}
export function tooltip(node: HTMLElement, param: TooltipParam) {
  ensureStyles();
  let opts = normalize(param);
  const id = `gm-tt-${++uid}`;
  let bubble: HTMLDivElement | null = null;
  let prevDescribedBy: string | null = null;
  let touchOpen = false;
  let autoDismissTimer = 0;
  let hoverTimer = 0;
  let focusFrame = 0;
  // Non-interactive jargon terms still need keyboard focus for aria-describedby.
  if (!node.hasAttribute("tabindex") && node.tabIndex < 0) {
    node.tabIndex = 0;
  }
  // Centred on the anchor, flipped to the opposite side when the requested one overflows the viewport, then clamped.
  function place() {
    if (!bubble) return;
    const anchor = node.getBoundingClientRect();
    const bubbleBox = bubble.getBoundingClientRect();
    const margin = 8;
    const anchorCenterX = anchor.left + anchor.width / 2;
    let top =
      opts.placement === "bottom"
        ? anchor.bottom + margin
        : anchor.top - bubbleBox.height - margin;
    if (top < margin) top = anchor.bottom + margin;
    if (top + bubbleBox.height > window.innerHeight - margin)
      top = anchor.top - bubbleBox.height - margin;
    let left = anchorCenterX - bubbleBox.width / 2;
    left = Math.max(
      margin,
      Math.min(left, window.innerWidth - bubbleBox.width - margin),
    );
    bubble.style.top = `${Math.max(margin, top)}px`;
    bubble.style.left = `${left}px`;
  }
  function show() {
    if (opts.disabled || bubble || !opts.text) return;
    bubble = document.createElement("div");
    bubble.className = "gm-tooltip";
    bubble.id = id;
    bubble.setAttribute("role", "tooltip");
    bubble.textContent = opts.text;
    document.body.appendChild(bubble);
    prevDescribedBy = node.getAttribute("aria-describedby");
    node.setAttribute(
      "aria-describedby",
      prevDescribedBy ? `${prevDescribedBy} ${id}` : id,
    );
    place();
    requestAnimationFrame(() => bubble?.setAttribute("data-show", "true"));
    for (const [target, type, listener, capture] of dismissListeners)
      target.addEventListener(type, listener as EventListener, capture);
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
    if (focusFrame) {
      cancelAnimationFrame(focusFrame);
      focusFrame = 0;
    }
    if (autoDismissTimer) {
      clearTimeout(autoDismissTimer);
      autoDismissTimer = 0;
    }
    if (!bubble) return;
    bubble.remove();
    bubble = null;
    if (prevDescribedBy === null) node.removeAttribute("aria-describedby");
    else node.setAttribute("aria-describedby", prevDescribedBy);
    prevDescribedBy = null;
    touchOpen = false;
    for (const [target, type, listener, capture] of dismissListeners)
      target.removeEventListener(type, listener as EventListener, capture);
  }
  function onKeydown(event: KeyboardEvent) {
    if (event.key === "Escape" && (bubble || focusFrame)) {
      hide();
    }
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
    clearHoverTimer();
    hide();
  }
  // Keyboard focus asks for the tip; focus landing from a click does not.
  function onFocus() {
    if (!node.matches(":focus-visible")) return;
    // Let focus reveal and its queued scroll event settle across rendering
    // before subscribing to dismissal, including resized history details.
    focusFrame = requestAnimationFrame(() => {
      focusFrame = requestAnimationFrame(() => {
        focusFrame = 0;
        if (document.activeElement === node && node.matches(":focus-visible"))
          show();
      });
    });
  }
  function onBlur() {
    hide();
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
    if (touchOpen) return;
    hide();
  }
  const dismissListeners = [
    [window, "blur", hide, false],
    [document, "visibilitychange", onVisibilityDismiss, false],
    [document, "scroll", hide, true],
    [document, "pointerdown", onDocumentPointerDown, true],
  ] as const;
  const nodeListeners = [
    ["pointerdown", onPointerDown],
    ["pointerenter", onPointerEnter],
    ["pointerleave", onPointerLeave],
    ["focus", onFocus],
    ["blur", onBlur],
    ["pointerup", onPointerUp],
    ["keydown", onKeydown],
    ["click", onClick],
  ] as const;
  for (const [type, listener] of nodeListeners)
    node.addEventListener(type, listener as EventListener);
  return {
    update(next: TooltipParam) {
      opts = normalize(next);
      if (bubble) {
        if (opts.disabled || !opts.text) hide();
        else {
          bubble.textContent = opts.text;
          place();
        }
      }
    },
    destroy() {
      hide();
      for (const [type, listener] of nodeListeners)
        node.removeEventListener(type, listener as EventListener);
    },
  };
}
export const JARGON = {
  bufferbloat:
    "Largest stage median RTT increase over idle. Includes browser, server and network delay.",
  jitter:
    "RTT variation: average absolute change between consecutive successful replies in one segment. Lower is steadier; timeouts are excluded.",
  p95: "95% of successful replies had an RTT at or below this value.",
  p50: "Median RTT: half of successful replies were faster, half slower.",
  p10: "10% of successful replies had an RTT at or below this value.",
  p90: "90% of successful replies had an RTT at or below this value.",
  probeTimeouts:
    "Timed-out replies as a share of resolved probes, not IP packet loss. Interrupted probes and failed sends are excluded.",
  wireRate:
    "Estimated physical-link rate, including forward-path protocol overhead.",
  stability: "How steady the measured speed was. Higher means less variation.",
  ping: "Round-trip time to the server and back. Lower is faster.",
  overheadCompensation:
    "Adds forward-path framing and protocol headers using detected transport and IP, or conservative defaults.",
  unitBits: "Bits per second (Mbit/s, Gbit/s), used by internet plans.",
  unitBytes: "MB/s or GB/s, used by download managers. One byte is eight bits.",
  unitDecimal: "Decimal prefixes: 1,000 per step (kbit/s, Mbit/s, Gbit/s).",
  unitBinary: "Binary prefixes: 1,024 per step (Kibit/s, Mibit/s, Gibit/s).",
} as const;
