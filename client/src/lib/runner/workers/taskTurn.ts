/** Yield to everything else queued in this realm, which a microtask checkpoint
 *  does not do. The datagram loops take a turn on an interval so their `stop`
 *  can land.
 *
 *  The port hop keeps the timer cheap: armed from a timer's own task it crosses
 *  the HTML nesting threshold and is floored at 4ms, the same order as the gap
 *  the loops yield on. Armed from a port task it carries no floor — 4.10ms
 *  against 0.007ms per turn in Chromium, 4.03ms against 0.033ms in Firefox.
 *  Both hops rather than the port alone: they are separate task sources, and a
 *  runtime is free to let a port loop starve timers. */
export function taskTurn(): Promise<void> {
  return new Promise((resolve) => {
    const { port1, port2 } = new MessageChannel();
    port1.onmessage = (): void => {
      port1.close();
      port2.close();
      setTimeout(resolve);
    };
    port2.postMessage(0);
  });
}
