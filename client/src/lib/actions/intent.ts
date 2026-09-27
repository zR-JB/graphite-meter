// Tips and plot readouts answer a pointer that comes to rest, not one passing by.
const WARM_MS = 500;
let warm = false;
let cooling = 0;

/** A tip or readout just closed: the next rest nearby answers almost at once. */
export function warmUp() {
  warm = true;
  clearTimeout(cooling);
  // Not motion: the warm spell ends.
  cooling = window.setTimeout(() => (warm = false), WARM_MS);
}

/** Calls `onRest` once the pointer stays within `radius` px for `ms`; a larger move restarts the wait. */
export function restDetector(onRest: () => void, radius = 4) {
  let timer = 0;
  let x = NaN;
  let y = NaN;
  function cancel() {
    clearTimeout(timer);
    timer = 0;
    x = y = NaN;
  }
  return {
    move(event: PointerEvent, ms: number) {
      if (Math.hypot(event.clientX - x, event.clientY - y) <= radius) return;
      clearTimeout(timer);
      x = event.clientX;
      y = event.clientY;
      // Not motion: intent is judged by rest time.
      timer = window.setTimeout(
        () => {
          cancel();
          onRest();
        },
        warm && event.pointerType === "mouse" ? Math.min(ms, 100) : ms,
      );
    },
    cancel,
  };
}
