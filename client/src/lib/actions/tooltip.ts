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
      padding: 8px 10px;
      border: 1px solid var(--border-strong);
      border-radius: var(--r-chrome);
      background: var(--surface-2);
      color: var(--text);
      box-shadow: var(--shadow-float);
      font-family: var(--font-sans);
      font-size: 12px;
      line-height: 1.4;
      font-weight: 500;
      letter-spacing: 0;
      text-transform: none;
      white-space: pre-line;
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
    if (event.key === "Escape" && bubble) {
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
    show();
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
    "Added latency under load: the largest measured stage median increase over idle. The grade summarizes that application round-trip delay; it includes browser, server, and network scheduling.",
  jitter:
    "RTT variation: the average absolute difference between consecutive successful replies in the same measurement segment. Lower is steadier. Timeouts carry no RTT.",
  p95: "P95: 95% of your pings were at or below this value. It captures the occasional slow spike rather than the typical case.",
  p50: "P50 (median): half your pings were faster than this, half slower — the typical ping.",
  p10: "P10: 10% of pings were at or below this — your best, quietest moments.",
  p90: "P90: 90% of pings were at or below this — captures the slower spikes.",
  probeTimeouts:
    "Probe timeouts: the share of resolved application probes whose reply deadline expired. This does not identify physical or directional IP packet loss. Interrupted probes and locally rejected sends are excluded.",
  wireRate:
    "Estimated forward-path physical-link occupancy for measured application bytes.",
  stability:
    "Stability: how steady the speed held during the test. Higher means a flat, consistent line; lower means it fluctuated.",
  ping: "Ping: the round-trip time for a small message to reach the server and come back. Lower feels snappier.",
  overheadCompensation:
    "Wire estimation adds only forward-path protocol bytes: Ethernet, IP, transport, TLS/QUIC, and HTTP framing. It uses negotiated protocol and authoritative preflight IP evidence, with conservative defaults when either is unavailable. It never guesses from stability, loss, browser cost, or ramp-up.",
  unitBits:
    "Bits per second — Mbit/s, Gbit/s. How internet plans are sold, so this is what you compare against your contract.",
  unitBytes:
    "Bytes per second — MB/s, GB/s. How download managers report speed. One byte is eight bits, so these numbers are 8× smaller.",
  unitDecimal:
    "SI prefixes, 1000 per step — kbit/s, Mbit/s, Gbit/s. The convention for network rates.",
  unitBinary:
    "IEC prefixes, 1024 per step — Kibit/s, Mibit/s, Gibit/s. The convention for memory and file sizes; the same speed reads about 2.4% lower per step than decimal.",
} as const;
