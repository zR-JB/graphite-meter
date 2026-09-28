// Tips and plot readouts answer a pointer that pauses on them, not one passing by.
const WARM_MS = 600;
let warm = false;
let cooling = 0;
let open: (() => void) | null = null;

/** A tip or readout just closed: the next one nearby answers at once. */
export function warmUp() {
  warm = true;
  clearTimeout(cooling);
  // Not motion: the warm spell ends.
  cooling = window.setTimeout(() => (warm = false), WARM_MS);
}

/** While a tip is open or one just closed, moving to the next answers without a pause. */
export const isWarm = () => warm || open !== null;

/** One tip at a time: opening one closes the last at once. */
export function claim(close: () => void) {
  if (open !== close) open?.();
  open = close;
}

export function release(close: () => void) {
  if (open === close) open = null;
}
