/* ============================================================
 * tooltip — Svelte action (§14.3, Batch J)
 * A small, reusable jargon-tooltip mechanism. Attach to any
 * element carrying a technical term; on BOTH hover and keyboard
 * focus it surfaces a plain-language definition in a token-styled
 * floating bubble. Accessible: the bubble gets `role="tooltip"`
 * and the trigger is wired to it via `aria-describedby`, so screen
 * readers announce the definition. Esc dismisses. Pure DOM, no
 * SvelteKit — mirrors the focusTrap / pointerIntent action style.
 *
 * Reduced-motion safe: the fade-in is gated on no-preference via
 * the injected stylesheet (the global §4.5 guard also neutralizes
 * any residual transition).
 *
 * Usage:  <span use:tooltip={"P95: 95% of pings were at or below this."}>P95</span>
 *         <button use:tooltip={{ text, placement: "bottom" }}>…</button>
 * ============================================================ */

export interface TooltipOptions {
  text: string;
  /** Preferred side; flips automatically if it would clip. Default "top". */
  placement?: "top" | "bottom";
  /** Disable without removing the action. */
  disabled?: boolean;
}

type TooltipParam = string | TooltipOptions;

let uid = 0;
const STYLE_ID = "gm-tooltip-styles";

/** Inject the tooltip stylesheet once (tokens only — no hardcoded hex). */
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
      border-radius: var(--radius-sm);
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

  // The trigger must be focusable so keyboard users can reach the definition.
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
      opts.placement === "bottom" ? r.bottom + margin : r.top - b.height - margin;
    // Flip if it would clip the viewport top/bottom.
    if (top < margin) top = r.bottom + margin;
    if (top + b.height > window.innerHeight - margin)
      top = r.top - b.height - margin;
    let left = cx - b.width / 2;
    left = Math.max(margin, Math.min(left, window.innerWidth - b.width - margin));
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
    // Next frame so the transition plays from the initial state.
    requestAnimationFrame(() => bubble?.setAttribute("data-show", "true"));
  }

  function hide() {
    if (!bubble) return;
    bubble.remove();
    bubble = null;
    if (prevDescribedBy === null) node.removeAttribute("aria-describedby");
    else node.setAttribute("aria-describedby", prevDescribedBy);
    prevDescribedBy = null;
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Escape" && bubble) {
      hide();
    }
  }

  node.addEventListener("pointerenter", show);
  node.addEventListener("pointerleave", hide);
  node.addEventListener("focus", show);
  node.addEventListener("blur", hide);
  node.addEventListener("keydown", onKeydown);

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
      node.removeEventListener("pointerenter", show);
      node.removeEventListener("pointerleave", hide);
      node.removeEventListener("focus", show);
      node.removeEventListener("blur", hide);
      node.removeEventListener("keydown", onKeydown);
    },
  };
}

/* ============================================================
 * Jargon dictionary — plain-language definitions for the
 * technical terms surfaced across the UI. Centralized so every
 * tooltip reads the same wording. Imported wherever a term shows.
 * ============================================================ */
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
    "Wire-rate: an estimate of your raw line speed before the unavoidable overhead of internet protocols. The measured number is what apps actually get; this is the theoretical ceiling.",
  stability:
    "Stability: how steady the speed held during the test. Higher means a flat, consistent line; lower means it fluctuated.",
  ping: "Ping: the round-trip time for a small message to reach the server and come back. Lower feels snappier.",
  overheadCompensation:
    "Overhead compensation: estimates your true wire-rate by adding back the bytes the browser never sees but your link still carries — Ethernet/IP/TCP framing, TLS records, HTTP framing, and return-path ACKs. Shown as the “wire” figure on the result cards; your measured speed is unchanged. At a 1500-byte MTU the exact protocol factors add roughly +3–8% combined; the heuristic factors add a few % more when on. How much depends on MTU, TCP/TLS/frame sizes and the transport, plus (for the heuristics) measured loss, stability and peak-vs-average.",
  compProtocol:
    "Protocol bytes — exact, fixed byte accounting from the protocol headers (high confidence). Adds the framing every packet carries at your MTU; typically a few percent each.",
  compPath:
    "Path behavior — heuristic estimates (low confidence): return-path ACK/control traffic, and a loss/retransmission tax ≈ loss ÷ (1 − loss) from measured loss, capped by Max loss ratio.",
  compModel:
    "Measurement model — heuristic estimates (low confidence) of how the measurement undercounts: the download-only browser receive-cost bias (grows with rate, ~0 at low speed), the early-test ramp toward the plateau (peak-vs-average), and browser/runtime timing jitter (derived from stability).",
  compProfile:
    "Connection profile — picks which overheads physically apply to your path and seeds sensible defaults. Local Ethernet adds full Ethernet/IP/TCP framing; Loopback adds none (no wire); VPN tunnel adds ~60 B/packet of WireGuard/Tailscale encapsulation on a smaller MTU; Internet follows your transport choice. You can still fine-tune every value under Advanced.",
  compTransport:
    "Transport & security — gates the TLS-record and HTTP-framing byte math. HTTP/1.1 cleartext adds neither; HTTPS adds TLS records; HTTP/2 adds TLS + DATA-frame headers; HTTP/3 swaps TCP for QUIC over UDP (its own packet framing, ~1.6% more than HTTP/2-over-TLS). Only HTTP/1.1 cleartext is wired today; the rest pre-configure the estimator for the TLS/HTTP-3 stage.",
} as const;
