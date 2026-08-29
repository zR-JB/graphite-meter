import type { Phase } from "../runner/contract";

export type ReturnToLive = {
  icon: "bolt" | "ping" | "download" | "upload" | "bidirectional";
  label: string;
  tone: "warmup" | "latency" | "download" | "upload" | "bidirectional";
};

export function returnToLiveIndicator(
  preparing: boolean,
  phase: Phase,
  recovering: boolean,
): ReturnToLive | null {
  if (preparing)
    return { icon: "bolt", label: "Starting test", tone: "warmup" };
  switch (phase) {
    case "connecting":
      return { icon: "bolt", label: "Verifying path", tone: "warmup" };
    case "warmup":
      return { icon: "bolt", label: "Warming up", tone: "warmup" };
    case "latency":
      return {
        icon: "ping",
        label: recovering ? "Latency recovering" : "Measuring latency",
        tone: "latency",
      };
    case "download":
      return {
        icon: "download",
        label: recovering ? "Download recovering" : "Downloading",
        tone: "download",
      };
    case "upload":
      return {
        icon: "upload",
        label: recovering ? "Upload recovering" : "Uploading",
        tone: "upload",
      };
    case "bidirectional":
      return {
        icon: "bidirectional",
        label: recovering ? "Bi-dir recovering" : "Bidirectional",
        tone: "bidirectional",
      };
    default:
      return null;
  }
}
