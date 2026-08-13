export interface ChartViewport {
  tMin: number;
  tMax: number;
  bytesPerSecMax: number;
  rttMin: number;
  rttMax: number;
}

export interface ChartLayout {
  width: number;
  height: number;
  plot: { left: number; right: number; top: number; bottom: number };
  viewport: ChartViewport;
  timeMinorTicks: ReadonlyArray<{ t: number; x: number }>;
  timeMajorTicks: ReadonlyArray<{ t: number; x: number }>;
  horizontalMinorLines: ReadonlyArray<number>;
  horizontalMajorLines: ReadonlyArray<number>;
  axisRows: ReadonlyArray<{ fraction: number; y: number }>;
  x(t: number): number;
  throughputY(bytesPerSec: number): number;
  latencyY(rttMs: number): number;
}

export const CHART_PADDING = { left: 46, right: 46, top: 12, bottom: 18 };

function niceTimeStep(target: number): number {
  const steps = [1000, 2000, 5000, 10000, 20000, 30000, 60000];
  for (const step of steps) if (step >= target) return step;
  return 60000;
}

/** CSS-pixel geometry for every chart canvas path and DOM text anchor. */
export function chartLayout(
  width: number,
  height: number,
  viewport: ChartViewport,
): ChartLayout {
  const safeWidth = Math.max(1, width);
  const safeHeight = Math.max(1, height);
  const left = Math.min(CHART_PADDING.left, safeWidth - 1);
  const top = Math.min(CHART_PADDING.top, safeHeight - 1);
  const plot = {
    left,
    right: Math.max(left + 1, safeWidth - CHART_PADDING.right),
    top,
    bottom: Math.max(top + 1, safeHeight - CHART_PADDING.bottom),
  };
  const plotWidth = plot.right - plot.left;
  const plotHeight = plot.bottom - plot.top;
  const timeSpan = viewport.tMax - viewport.tMin || 1;
  const latencySpan = viewport.rttMax - viewport.rttMin || 1;
  const x = (t: number) =>
    plot.left + ((t - viewport.tMin) / timeSpan) * plotWidth;
  const throughputY = (bytesPerSec: number) =>
    plot.top + (1 - bytesPerSec / viewport.bytesPerSecMax) * plotHeight;
  const latencyY = (rttMs: number) =>
    Math.max(
      plot.top,
      Math.min(
        plot.bottom,
        plot.top + (1 - (rttMs - viewport.rttMin) / latencySpan) * plotHeight,
      ),
    );
  const majorStep = niceTimeStep(timeSpan / 5);
  const minorStep = majorStep / 4;
  const timeTicks = (step: number) => {
    const ticks: Array<{ t: number; x: number }> = [];
    for (
      let t = Math.ceil(viewport.tMin / step) * step;
      t <= viewport.tMax;
      t += step
    ) {
      const tickX = Math.round(x(t)) + 0.5;
      if (tickX >= plot.left && tickX <= plot.right)
        ticks.push({ t, x: tickX });
    }
    return ticks;
  };
  const horizontalMinorLines = Array.from(
    { length: 15 },
    (_, index) => index + 1,
  )
    .filter((index) => index % 4 !== 0)
    .map((index) => Math.round(plot.top + (plotHeight * index) / 16) + 0.5);
  const horizontalMajorLines = Array.from(
    { length: 3 },
    (_, index) => Math.round(plot.top + (plotHeight * (index + 1)) / 4) + 0.5,
  );
  const axisRows = Array.from({ length: 3 }, (_, index) => {
    const fraction = index / 2;
    return { fraction, y: plot.top + plotHeight * fraction };
  });

  return {
    width: safeWidth,
    height: safeHeight,
    plot,
    viewport,
    timeMinorTicks: timeTicks(minorStep),
    timeMajorTicks: timeTicks(majorStep),
    horizontalMinorLines,
    horizontalMajorLines,
    axisRows,
    x,
    throughputY,
    latencyY,
  };
}
