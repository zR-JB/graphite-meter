import type { Phase } from "../runner/contract";
import { phaseLabel } from "../presentation/vocabulary";

type Tone = "warmup" | "latency" | "download" | "upload" | "bidirectional";
type ReturnToLive = {
  icon: "bolt" | "ping" | "download" | "upload" | "bidirectional";
  label: string;
  tone: Tone;
};

export function returnToLiveIndicator(
  preparing: boolean,
  phase: Phase,
  recovering: boolean,
): ReturnToLive | null {
  if (preparing || phase === "connecting" || phase === "warmup")
    return {
      icon: "bolt",
      label: phaseLabel(preparing ? "connecting" : phase),
      tone: "warmup",
    };
  if (
    phase !== "latency" &&
    phase !== "download" &&
    phase !== "upload" &&
    phase !== "bidirectional"
  )
    return null;
  return {
    icon: phase === "latency" ? "ping" : phase,
    label: `${phaseLabel(phase)}${recovering ? " · recovering" : ""}`,
    tone: phase,
  };
}
