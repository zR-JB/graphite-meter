// A combined value needs both lanes; a surviving lane stays directional evidence, even a zero.
export function bidirectionalResultPresentation(
  down: number | null | undefined,
  up: number | null | undefined,
): {
  down: number | null;
  up: number | null;
  combinedBytesPerSec: number | null;
  survivingDirection: "down" | "up" | null;
} {
  return {
    down: down ?? null,
    up: up ?? null,
    combinedBytesPerSec: down != null && up != null ? down + up : null,
    survivingDirection:
      down != null && up == null
        ? "down"
        : up != null && down == null
          ? "up"
          : null,
  };
}
