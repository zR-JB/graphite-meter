export const MIN_DOCK_WIDTH = 320;
export const MAX_DOCK_WIDTH = 720;
// Includes stage padding, keeping the gauge and latency instruments legible.
export const MIN_STAGE_WIDTH = 800;

export function resolveDockWidths(width: number, left: number, right: number) {
  const clamp = (value: number) =>
    value > 0 ? Math.max(MIN_DOCK_WIDTH, Math.min(MAX_DOCK_WIDTH, value)) : 0;
  left = clamp(left);
  right = clamp(right);
  const count = Number(left > 0) + Number(right > 0);
  const budget = Math.max(count * MIN_DOCK_WIDTH, width - MIN_STAGE_WIDTH);
  if (left + right <= budget) return { left, right };
  const extra = budget - count * MIN_DOCK_WIDTH;
  const preferredExtra = left + right - count * MIN_DOCK_WIDTH;
  const resolvedLeft = left
    ? MIN_DOCK_WIDTH +
      Math.floor((extra * (left - MIN_DOCK_WIDTH)) / preferredExtra)
    : 0;
  return { left: resolvedLeft, right: right ? budget - resolvedLeft : 0 };
}
