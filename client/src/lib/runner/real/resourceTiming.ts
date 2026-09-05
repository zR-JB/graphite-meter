const DELIVERY_BUDGET_MS = 250;

/** Body completion can precede delivery of the browser's Resource Timing entry. */
export async function resourceProtocol(
  url: string,
  signal?: AbortSignal,
): Promise<string | undefined> {
  signal?.throwIfAborted();
  const existing = performance.getEntriesByName(url, "resource").at(-1) as
    PerformanceResourceTiming | undefined;
  if (existing) return existing.nextHopProtocol || undefined;
  if (typeof PerformanceObserver === "undefined") return undefined;

  return new Promise((resolve, reject) => {
    const observer = new PerformanceObserver((list) => {
      const entry = list.getEntriesByName(url).at(-1) as
        PerformanceResourceTiming | undefined;
      if (entry) finish(entry.nextHopProtocol || undefined);
    });
    const cleanup = () => {
      observer.disconnect();
      clearTimeout(timer);
      signal?.removeEventListener("abort", abort);
    };
    const finish = (protocol?: string) => {
      cleanup();
      resolve(protocol);
    };
    const abort = () => {
      cleanup();
      reject(signal?.reason);
    };
    const timer = setTimeout(() => finish(), DELIVERY_BUDGET_MS);
    signal?.addEventListener("abort", abort, { once: true });
    try {
      observer.observe({ type: "resource", buffered: true });
    } catch {
      finish();
    }
  });
}
