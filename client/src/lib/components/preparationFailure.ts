import type { PreparationState } from "../state/store.svelte";

interface PreparationFailurePresentation {
  headline: string;
  detail: string;
}

/* Keep preflight failures readable without exposing transport diagnostics in the primary gauge message. */
export function preparationFailurePresentation(
  preparation: Pick<PreparationState, "status" | "throughput" | "latency">,
  startError: string,
): PreparationFailurePresentation | null {
  if (preparation.status !== "failed") return null;
  const failedThroughput = preparation.throughput === "failed";
  const failedLatency = preparation.latency === "failed";
  if (failedThroughput || failedLatency) {
    const detail =
      failedThroughput && failedLatency
        ? "Throughput and latency paths are unavailable"
        : failedThroughput
          ? "Throughput path is unavailable"
          : "Latency path is unavailable";
    return { headline: "Connection check failed", detail };
  }
  return {
    headline: "Test cannot start",
    detail: startError || "Unable to start the test",
  };
}
