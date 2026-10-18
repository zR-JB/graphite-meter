// Timeouts that hidden-tab throttling cannot stretch: post {id, ms} (null ms cancels), get the id back on firing.
const timers = new Map<number, ReturnType<typeof setTimeout>>();

self.onmessage = ({ data: { id, ms } }: MessageEvent<{ id: number; ms: number | null }>) => {
  clearTimeout(timers.get(id));
  timers.delete(id);
  if (ms !== null)
    timers.set(
      id,
      setTimeout(() => {
        timers.delete(id);
        postMessage(id);
      }, ms),
    );
};
