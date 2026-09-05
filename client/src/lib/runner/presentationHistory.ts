import type { LatencyBucket, ThroughputSample } from "./contract";

export const PRESENTATION_POINT_LIMIT = 1_200;

const throughputKey = (sample: ThroughputSample): string =>
  `${sample.phase}:${sample.dir}:${sample.continuityId}`;

export function appendThroughputSample(
  history: ThroughputSample[],
  sample: ThroughputSample,
  limit = PRESENTATION_POINT_LIMIT,
  targetSpanMs = 0,
): boolean {
  history.push(sample);
  if (history.length <= limit) return false;
  return compactThroughputHistory(
    history,
    Math.max(targetSpanMs, history.at(-1)!.t - history[0].t),
    limit,
  );
}

export function compactThroughputHistory(
  history: ThroughputSample[],
  targetSpanMs: number,
  limit = PRESENTATION_POINT_LIMIT,
): boolean {
  if (history.length <= 1 || limit <= 0) {
    const changed = history.length > Math.max(0, limit);
    if (changed) history.splice(0, history.length);
    return changed;
  }
  const series = new Set(history.map(throughputKey)).size;
  const span = Math.max(1, targetSpanMs, history.at(-1)!.t - history[0].t);
  let width = Math.max(1, span / Math.max(1, Math.floor(limit / (series * 2))));
  let reduced = reduceThroughput(history, width);
  while (reduced.length > limit && width < span) {
    width *= 2;
    reduced = reduceThroughput(history, width);
  }
  if (reduced.length > limit) reduced = evenlySpaced(reduced, limit);
  if (
    reduced.length === history.length &&
    reduced.every((sample, index) => sample === history[index])
  )
    return false;
  history.splice(0, history.length, ...reduced);
  return true;
}

function reduceThroughput(
  history: readonly ThroughputSample[],
  width: number,
): ThroughputSample[] {
  const bins = new Map<
    string,
    { min: ThroughputSample; max: ThroughputSample }
  >();
  const first = new Map<string, ThroughputSample>();
  const last = new Map<string, ThroughputSample>();
  for (const sample of history) {
    const series = throughputKey(sample);
    if (!first.has(series)) first.set(series, sample);
    last.set(series, sample);
    const key = `${series}:${Math.floor(sample.t / width)}`;
    const bin = bins.get(key);
    if (bin) {
      if (sample.bytesPerSec < bin.min.bytesPerSec) bin.min = sample;
      if (sample.bytesPerSec > bin.max.bytesPerSec) bin.max = sample;
    } else bins.set(key, { min: sample, max: sample });
  }
  const selected = new Set([...first.values(), ...last.values()]);
  for (const { min, max } of bins.values()) {
    selected.add(min);
    selected.add(max);
  }
  return history.filter((sample) => selected.has(sample));
}

function evenlySpaced<T>(values: readonly T[], limit: number): T[] {
  if (limit <= 0) return [];
  if (limit === 1) return values.length ? [values.at(-1)!] : [];
  return Array.from(
    { length: limit },
    (_, i) => values[Math.round((i * (values.length - 1)) / (limit - 1))],
  );
}

const latencyKey = (sample: LatencyBucket): string =>
  `${sample.phase}:${sample.underLoad}:${sample.continuityId}`;

export function compactLatencyHistory(
  history: LatencyBucket[],
  limit = PRESENTATION_POINT_LIMIT,
): void {
  if (history.length <= limit) return;
  const series = new Set(history.map(latencyKey)).size;
  const span = Math.max(1, history.at(-1)!.endT - history[0].startT);
  let width = Math.max(1, span / Math.max(1, Math.floor(limit / series)));
  let reduced = reduceLatency(history, width);
  while (reduced.length > limit && width < span) {
    width *= 2;
    reduced = reduceLatency(history, width);
  }
  if (reduced.length > limit) reduced = evenlySpaced(reduced, limit);
  history.splice(0, history.length, ...reduced);
}

function reduceLatency(
  history: readonly LatencyBucket[],
  width: number,
): LatencyBucket[] {
  const bins = new Map<string, LatencyBucket[]>();
  for (const sample of history) {
    const key = `${latencyKey(sample)}:${Math.floor(sample.startT / width)}`;
    const bin = bins.get(key);
    if (bin) bin.push(sample);
    else bins.set(key, [sample]);
  }
  return [...bins.values()]
    .map(mergeLatency)
    .sort((a, b) => a.startT - b.startT);
}

function mergeLatency(samples: readonly LatencyBucket[]): LatencyBucket {
  const first = samples[0];
  const last = samples.at(-1)!;
  const successes = samples.reduce(
    (sum, sample) => sum + sample.pingCount - sample.lossCount,
    0,
  );
  let medianRttMs: number | null = null;
  let seen = 0;
  for (const sample of samples) {
    seen += sample.pingCount - sample.lossCount;
    if (sample.medianRttMs != null && seen >= successes / 2) {
      medianRttMs = sample.medianRttMs;
      break;
    }
  }
  let rttDeltaSumMs = samples.reduce(
    (sum, sample) => sum + sample.rttDeltaSumMs,
    0,
  );
  let rttDeltaCount = samples.reduce(
    (sum, sample) => sum + sample.rttDeltaCount,
    0,
  );
  let previous = first.lastRttMs;
  for (const sample of samples.slice(1)) {
    if (previous != null && sample.firstRttMs != null) {
      rttDeltaSumMs += Math.abs(sample.firstRttMs - previous);
      rttDeltaCount++;
    }
    if (sample.lastRttMs != null) previous = sample.lastRttMs;
  }
  const finite = (pick: (sample: LatencyBucket) => number | null): number[] =>
    samples.flatMap((sample) => {
      const value = pick(sample);
      return value == null ? [] : [value];
    });
  const p95 = finite((sample) => sample.p95RttMs);
  const maxima = finite((sample) => sample.maxRttMs);
  return {
    ...first,
    t: first.startT + (last.endT - first.startT) / 2,
    endT: last.endT,
    medianRttMs,
    p95RttMs: p95.length ? Math.max(...p95) : null,
    maxRttMs: maxima.length ? Math.max(...maxima) : null,
    firstRttMs:
      samples.find((sample) => sample.firstRttMs != null)?.firstRttMs ?? null,
    lastRttMs:
      samples.findLast((sample) => sample.lastRttMs != null)?.lastRttMs ?? null,
    rttDeltaSumMs,
    rttDeltaCount,
    pingCount: samples.reduce((sum, sample) => sum + sample.pingCount, 0),
    lossCount: samples.reduce((sum, sample) => sum + sample.lossCount, 0),
  };
}
