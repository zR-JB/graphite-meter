/// <reference lib="webworker" />
// Timers the page borrows: a tab hidden for minutes wakes its own timers about once a minute, never a worker's.
const timers = new Map<number, ReturnType<typeof setTimeout>>();

self.onmessage = ({
  data,
}: MessageEvent<{ id: number; ms: number | null }>) => {
  clearTimeout(timers.get(data.id));
  timers.delete(data.id);
  if (data.ms === null) return;
  timers.set(
    data.id,
    setTimeout(() => {
      timers.delete(data.id);
      self.postMessage(data.id);
    }, data.ms),
  );
};
