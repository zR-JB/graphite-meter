// Runs timeouts for the page that hidden-tab throttling cannot stretch. The owner posts {id, ms} (ms null cancels)
// and gets the id back when it fires.
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
