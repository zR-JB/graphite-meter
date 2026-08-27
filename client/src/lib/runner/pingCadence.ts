import type { PingCadence } from "./contract";

/* Fixed start-to-start ping intervals shared by every runner and by the adaptive evidence policy. */
const FIXED_PING_INTERVAL_MS = {
  fast: 80,
  medium: 250,
  slow: 600,
} as const satisfies Record<Exclude<PingCadence, "reply-driven">, number>;

export function fixedPingIntervalMs(cadence: PingCadence): number | null {
  return cadence === "reply-driven" ? null : FIXED_PING_INTERVAL_MS[cadence];
}
