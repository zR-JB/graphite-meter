<script module lang="ts">
  // Module scope: returning from History must not announce the same run again.
  let spoken: unknown = null;
</script>

<script lang="ts">
  import ResultSummary from "./ResultSummary.svelte";
  import { store } from "../state/store.svelte";
  import { fmtBytes, fmtMs, formatRate, resultRate } from "../format";
  import { JARGON, MISSING, STAGE } from "../presentation/vocabulary";
  import type { LiveReadout } from "../presentation/liveReadout.svelte";
  import { announce } from "../presentation/announcer.svelte";
  import { handoff } from "../presentation/motion.svelte";
  import { replies } from "../presentation/stageGraph";
  import { untrack } from "svelte";
  import {
    CARD_ORDER,
    laneShort,
    resultSentence,
    summaryCards,
    serverIssues,
    summaryEvidence,
    type CardGraph,
    type CardScale,
    type SummaryCard,
    type SummaryRow,
  } from "../presentation/resultSummary";

  type Stage = (typeof CARD_ORDER)[number];

  let { live }: { live: LiveReadout } = $props();

  const shown = $derived(store.resultScope);
  const details = $derived(store.result?.multiServer);
  // One tier for every rate on the page, the dial's, so live and settled figures read in one unit.
  const units = $derived({
    base: store.unitBase,
    kind: store.unitKind,
    tier: store.scales.unitIndex,
  });
  const status = (key: Stage) => store.stagePresentation[key].status;
  // A stalled stage is still running: it keeps its plan's width and its leading edge.
  const running = (key: Stage) =>
    status(key) === "active" || status(key) === "recovering";

  const settled = $derived.by(() => {
    const evidence = summaryEvidence(
      Object.fromEntries(CARD_ORDER.map((key) => [key, status(key)])) as Record<
        Stage,
        string
      >,
      {
        download: store.stageResults.download,
        upload: store.stageResults.upload,
        bidirectional: store.result?.bidirectional ?? null,
        latency: store.stageResults.latency,
        added: store.result?.addedLatency ?? null,
      },
      details,
      shown,
    );
    return summaryCards(evidence, units, store.showWireEstimates);
  });
  // Until a run completes, every planned stage holds a card; values fill in, nothing moves.
  const planned = $derived(
    CARD_ORDER.filter((key) => status(key) !== "disabled"),
  );
  type Transfer = Exclude<Stage, "latency">;
  // The run's series per stage, over its plan until settled; a scoped server has no series of its own.
  const graphs = $derived.by(() => {
    if (shown) return {} as Partial<Record<Transfer, CardGraph>>;
    const plan = (store.run?.config ?? store.config).duration;
    const lane = (key: Transfer, dir: "down" | "up") =>
      store.throughput
        .filter((s) => s.phase === key && s.dir === dir)
        .map((s) => ({ t: s.t, v: s.bytesPerSec }));
    const graph = (key: Transfer): CardGraph => {
      const lanes =
        key === "bidirectional"
          ? [lane(key, "down"), lane(key, "up")]
          : [lane(key, key === "download" ? "down" : "up")];
      const times = lanes.flat().map((point) => point.t);
      const start = times.length ? Math.min(...times) : 0;
      const measured = times.length ? Math.max(...times) - start : 0;
      return {
        lanes,
        latency: replies(store.latency, key),
        start,
        span:
          Math.max(
            measured,
            running(key) || status(key) === "pending" ? plan[`${key}Ms`] : 0,
          ) || 1,
      };
    };
    return {
      download: graph("download"),
      upload: graph("upload"),
      bidirectional: graph("bidirectional"),
    };
  });
  // The running card's leading edge moves on the frame clock; the other graphs stay put.
  const head = $derived.by(() => {
    const key = live.phase;
    const rates = live.rates;
    if (!key || !rates || !running(key)) return null;
    return {
      key,
      t: store.phaseStartedAtMs + store.phaseClock.current,
      values:
        key === "bidirectional"
          ? [rates.down, rates.up]
          : [key === "download" ? rates.down : rates.up],
    };
  });
  const scale = $derived<CardScale>({
    ceiling: store.scales.chartBytesPerSec,
    baseline:
      store.latencyLanes.find((lane) => lane.key === "latency")?.center ?? null,
    latencyTop: store.latencyScaleMs,
    rate: (bytesPerSec) => formatRate(bytesPerSec, units),
  });
  const cards = $derived(
    (store.phase !== "complete" && store.phase !== "error"
      ? planned.map(
          (key) => settled.find((card) => card.key === key) ?? liveCard(key),
        )
      : settled
    ).map((card) => ({
      ...card,
      graph: graphs[card.key as Transfer] ?? null,
    })),
  );

  const view = handoff(
    () => ({
      run: store.runSeq,
      cards,
      // Lost latency probes are marked on the latency card's rows; nothing is added under the cards mid-run.
      issues: store.serverDetails
        ? serverIssues(store.serverDetails, shown).filter(
            (issue) => issue.throughput.length,
          )
        : [],
    }),
    (view) => view.run,
  );

  // Once per completed run, never again for a unit or scope change.
  $effect(() => {
    if (store.phase !== "complete" || store.result === spoken) return;
    spoken = store.result;
    announce(untrack(() => resultSentence(settled)));
  });

  // A running stage fills its facts as it goes: bytes so far, and each bidirectional lane's rate and their sum.
  function liveRows(key: Stage): SummaryRow[] {
    if (key === "latency") return [];
    const moved = {
      label: "Transferred",
      value: fmtBytes(store.liveStageBytes, units.base),
    };
    if (key !== "bidirectional") return [moved];
    const combined = live.rates && live.rates.down + live.rates.up;
    return [
      ...(["download", "upload"] as const).map((stage) => {
        const rate = live.rates?.[stage === "download" ? "down" : "up"];
        return {
          label: STAGE[stage].short,
          value: rate == null ? MISSING : formatRate(rate, units),
          short: laneShort(rate, combined, units),
          stage,
        };
      }),
      {
        label: "Down + up",
        value: combined == null ? MISSING : formatRate(combined, units),
      },
      moved,
    ];
  }

  // Animated values are visual only; the accessible value uses receiver accounting.
  function liveCard(key: Stage): SummaryCard {
    const active = running(key);
    // Warmup replies are not counted, so the card shows none, like the gauge.
    const own =
      active &&
      (key === "latency" ? store.phase === "latency" : live.phase === key);
    const timeout = key === "latency" && own && store.liveLatencyLost;
    const { down = null, up = null } =
      store.live?.phase === key ? store.live : {};
    const stopped = store.phase === "aborted" && store.phaseStage === key;
    const [value, accessible] = !own
      ? [null, null]
      : key === "latency"
        ? [live.rtt.current, store.liveRtt]
        : [
            live.rates!.down + live.rates!.up,
            down == null && up == null ? null : (down ?? 0) + (up ?? 0),
          ];
    // A value's unit arrives with it, so a pending card never shifts its unit.
    const readout = (n: number | null) =>
      n === null
        ? { num: MISSING, unit: "" }
        : key === "latency"
          ? { num: fmtMs(n), unit: "ms" }
          : resultRate(n, units);
    const shown = readout(value);
    const spoken = readout(accessible);
    return {
      key,
      label: STAGE[key].short,
      icon: STAGE[key].icon,
      status: active
        ? status(key) === "recovering"
          ? "recovering"
          : "active"
        : stopped
          ? "stopped"
          : store.phase === "aborted"
            ? "not-run"
            : "pending",
      num: timeout ? MISSING : shown.num,
      unit: timeout ? "timeout" : shown.unit,
      tip: JARGON[key],
      rows: own || stopped ? liveRows(key) : [],
      accessible: active
        ? timeout
          ? "probe timeout"
          : `${spoken.num} ${spoken.unit}`
        : undefined,
    };
  }
</script>

<ResultSummary
  cards={view.shown.cards}
  {scale}
  {head}
  fade={view.opacity}
  details={details ?? store.serverDetails}
  issues={view.shown.issues}
  scope={details ? shown : ""}
  running={store.isRunning}
/>
