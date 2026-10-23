// The page's failed WebTransport handshakes: Chrome delays every later one by those of the last 5 minutes.
import { signal, type Read } from "../../lib/signal";

const WINDOW_MS = 5 * 60_000;
/** Four failures cost at most about 120 ms of Chrome's delay; the next ones grow it exponentially. */
const ALLOWED = 4;

export const THROTTLED =
  "WebTransport failed 4 times in this page; Chrome now delays new attempts. Reload the page to reset it.";

export class HandshakeLedger {
  #failures: number[] = [];
  #lift: ReturnType<typeof setTimeout> | undefined;
  readonly #throttled = signal(false);
  readonly throttled: Read<boolean> = this.#throttled;

  constructor(readonly now: () => number = () => performance.now()) {}

  /** A WebTransport handshake failed or outlived its budget. */
  failed(): void {
    this.#failures.push(this.now());
    this.#settle();
  }

  /** Throttled while 4 failures lie within the window; lifts when the oldest of them leaves it. */
  #settle(): void {
    const now = this.now();
    // A stamp ahead of now comes from another clock, as each test's fake one, never from this page.
    this.#failures = this.#failures.filter((at) => at <= now && now - at < WINDOW_MS);
    const throttled = this.#failures.length >= ALLOWED;
    this.#throttled.set(throttled);
    clearTimeout(this.#lift);
    if (throttled)
      this.#lift = setTimeout(() => this.#settle(), this.#failures.at(-ALLOWED)! + WINDOW_MS - now);
  }
}

export const handshakes = new HandshakeLedger();
