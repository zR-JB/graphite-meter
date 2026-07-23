// Svelte tooltip action plus the shared jargon dictionary used by metric labels
// and settings controls.
const ACTIONABLE_SELECTOR = "button, a, label, [role='switch'], [role='tab']";

export interface TooltipOptions {
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

function ensureStyles() {
  if (typeof document === "undefined") return;
  if (document.getElementById(STYLE_ID)) return;
  const el = document.createElement("style");
  el.id = STYLE_ID;
  el.textContent = `
    .gm-tooltip {
      position: fixed;
      z-index: 200;
      max-width: 260px;
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
    .gm-tooltip__term {
      display: block;
      margin-bottom: 2px;
      font-family: var(--font-mono);
      font-size: 10px;
      font-weight: 700;
      letter-spacing: 0.04em;
      text-transform: uppercase;
      color: var(--brand-strong);
    }
  `;
  document.head.appendChild(el);
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

  // Non-interactive jargon terms still need keyboard focus for aria-describedby.
  if (!node.hasAttribute("tabindex") && node.tabIndex < 0) {
    node.tabIndex = 0;
  }

  function place() {
    if (!bubble) return;
    const r = node.getBoundingClientRect();
    const b = bubble.getBoundingClientRect();
    const margin = 8;
    const cx = r.left + r.width / 2;
    let top =
      opts.placement === "bottom"
        ? r.bottom + margin
        : r.top - b.height - margin;
    if (top < margin) top = r.bottom + margin;
    if (top + b.height > window.innerHeight - margin)
      top = r.top - b.height - margin;
    let left = cx - b.width / 2;
    left = Math.max(
      margin,
      Math.min(left, window.innerWidth - b.width - margin),
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
    window.addEventListener("blur", hide);
    document.addEventListener("visibilitychange", onVisibilityDismiss);
    document.addEventListener("scroll", hide, true);
    document.addEventListener("pointerdown", onDocumentPointerDown, true);
  }

  let touchOpen = false;
  let autoDismissTimer = 0;
  let hoverTimer = 0;

  function clearHoverTimer() {
    if (hoverTimer) {
      clearTimeout(hoverTimer);
      hoverTimer = 0;
    }
  }

  function onDocumentPointerDown(e: PointerEvent) {
    const target = e.target as Node | null;
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
    window.removeEventListener("blur", hide);
    document.removeEventListener("visibilitychange", onVisibilityDismiss);
    document.removeEventListener("scroll", hide, true);
    document.removeEventListener("pointerdown", onDocumentPointerDown, true);
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Escape" && bubble) {
      hide();
    }
  }

  function onPointerEnter(e: PointerEvent) {
    if (e.pointerType !== "mouse") return;
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
  function onPointerLeave(e: PointerEvent) {
    if (e.pointerType !== "mouse") return;
    clearHoverTimer();
    hide();
  }
  function onFocus() {
    if (!node.matches(":focus-visible")) return;
    show();
  }
  function onBlur() {
    hide();
  }

  function onPointerUp(e: PointerEvent) {
    if (e.pointerType !== "touch") return;
    if (touchOpen) {
      hide();
      return;
    }
    if (node.closest(ACTIONABLE_SELECTOR)) return;
    show();
    if (!bubble) return;
    touchOpen = true;
    autoDismissTimer = window.setTimeout(hide, 4000);
  }

  function onPointerDown(e: PointerEvent) {
    if (e.pointerType !== "touch") hide();
  }

  function onClick() {
    if (touchOpen) return;
    hide();
  }

  node.addEventListener("pointerdown", onPointerDown);
  node.addEventListener("pointerenter", onPointerEnter);
  node.addEventListener("pointerleave", onPointerLeave);
  node.addEventListener("focus", onFocus);
  node.addEventListener("blur", onBlur);
  node.addEventListener("pointerup", onPointerUp);
  node.addEventListener("keydown", onKeydown);
  node.addEventListener("click", onClick);

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
      node.removeEventListener("pointerdown", onPointerDown);
      node.removeEventListener("pointerenter", onPointerEnter);
      node.removeEventListener("pointerleave", onPointerLeave);
      node.removeEventListener("focus", onFocus);
      node.removeEventListener("blur", onBlur);
      node.removeEventListener("pointerup", onPointerUp);
      node.removeEventListener("keydown", onKeydown);
      node.removeEventListener("click", onClick);
    },
  };
}

export const JARGON = {
  bufferbloat:
    "Bufferbloat: extra delay that piles up when your connection is busy. A grade of A means it stays responsive under load; D or F means calls and games may lag during big downloads.",
  jitter:
    "Jitter: how much your ping bounces around from moment to moment. Lower is steadier — high jitter can make video calls choppy.",
  p95: "P95: 95% of your pings were at or below this value. It captures the occasional slow spike rather than the typical case.",
  p50: "P50 (median): half your pings were faster than this, half slower — the typical ping.",
  p10: "P10: 10% of pings were at or below this — your best, quietest moments.",
  p90: "P90: 90% of pings were at or below this — captures the slower spikes.",
  packetLoss:
    "Packet loss: the share of test pings that never came back. Even a few percent can stutter calls and streams.",
  wireRate:
    "Wire-rate: estimated forward-direction Ethernet occupancy for the measured application bytes. The range covers packet details browsers cannot inspect, such as TCP options. Reverse ACK traffic and runtime behavior are not added.",
  stability:
    "Stability: how steady the speed held during the test. Higher means a flat, consistent line; lower means it fluctuated.",
  ping: "Ping: the round-trip time for a small message to reach the server and come back. Lower feels snappier.",
  overheadCompensation:
    "Wire estimation adds only forward-path protocol bytes: Ethernet, IP, transport, TLS/QUIC, HTTP framing, and an explicitly configured tunnel. It never guesses from stability, loss, browser cost, or ramp-up.",
  compProfile:
    "Connection profile selects the physical first hop. Local Ethernet uses a 1500-byte MTU, loopback has no physical wire, and the tunnel preset uses a 1420-byte inner MTU with 60 bytes of encapsulation.",
  compTransport:
    "Automatic reads the browser-facing protocol from Resource Timing, so HTTPS and a reverse proxy are handled at the correct hop. Expert overrides are available for testing unusual HTTP/QUIC paths.",
} as const;
