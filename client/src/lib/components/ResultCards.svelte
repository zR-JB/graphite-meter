<script module lang="ts">
  // Module scope: returning from History must not announce the same run again.
  let spoken: unknown = null;
</script>

<script lang="ts">
  import ResultSummary from "./ResultSummary.svelte";
  import { store } from "../state/store.svelte";
  import { fmtBytes, fmtMs, formatRate, resultRate } from "../format";
  import { JARGON, MISSING, STAGE } from "../presentation/vocabulary";
  import { replies } from "../presentation/stageGraph";
  import { latencyTrackScale } from "../presentation/scales";
  import { stabilityPct } from "../runner/measure";
  import type { LiveReadout } from "../presentation/liveReadout.svelte";
  import { announce } from "../presentation/announcer.svelte";
  import { handoff } from "../presentation/motion.svelte";
  import { untrack } from "svelte";
  import {
    CARD_ORDER,
    laneShort,
    resultSentence,
    summaryCards,
    serverIssues,
    summaryEvidence,
    buildCardGraphs,
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
        idle: store.latencySummaries.latency ?? null,
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
  let retainedGraphs: Partial<Record<Transfer, CardGraph>> = {};
  // The run's series per stage, over its plan until settled; a scoped server has no series of its own.
  const graphs = $derived.by(() => {
    if (shown) return {} as Partial<Record<Transfer, CardGraph>>;
    const plan = (store.run?.config ?? store.config).duration;
    const span = (key: Transfer) =>
      running(key) || status(key) === "pending" ? plan[`${key}Ms`] : 0;
    return (retainedGraphs = buildCardGraphs(
      store.throughput,
      store.latency,
      {
        download: span("download"),
        upload: span("upload"),
        bidirectional: span("bidirectional"),
      },
      retainedGraphs,
    ));
  });
  // The idle replies span the planned stage while it may still run, then the time they took: the latency card's
  // strip.
  const latencyTrace = $derived.by<CardGraph>(() => {
    const points = replies(store.latency, "latency");
    const start = points[0]?.t ?? 0;
    const measured = (points.at(-1)?.t ?? start) - start;
    const settled = !running("latency") && status("latency") !== "pending";
    const plan = (store.run?.config ?? store.config).duration.latencyMs;
    return {
      lanes: [],
      latency: points,
      start,
      span: Math.max(measured, settled ? 0 : plan) || 1,
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
    // The tracks' top comes from the buckets' slowest replies, since a bar spans its bucket's range.
    latencyTop: latencyTrackScale(
      store.latency.map((bucket) => bucket.maxRttMs ?? bucket.medianRttMs),
    ),
    rate: (bytesPerSec) => formatRate(bytesPerSec, units),
  });
  // A stage subscribes to its own live value. Animating one must not rebuild
  // the pending and finished cards, their rows, or their tooltip parameters.
  const latencyCard = $derived(
    settled.find((card) => card.key === "latency") ?? liveCard("latency"),
  );
  const downloadCard = $derived(
    settled.find((card) => card.key === "download") ?? liveCard("download"),
  );
  const uploadCard = $derived(
    settled.find((card) => card.key === "upload") ?? liveCard("upload"),
  );
  const bidirectionalCard = $derived(
    settled.find((card) => card.key === "bidirectional") ??
      liveCard("bidirectional"),
  );
  const byStage = $derived({
    latency: latencyCard,
    download: downloadCard,
    upload: uploadCard,
    bidirectional: bidirectionalCard,
  });
  const retainedCards: Partial<
    Record<Stage, { source: SummaryCard; view: SummaryCard }>
  > = {};
  function withGraph(card: SummaryCard): SummaryCard {
    const graph =
      card.key === "latency"
        ? latencyTrace
        : (graphs[card.key as Transfer] ?? null);
    const previous = retainedCards[card.key];
    if (previous?.source === card && previous.view.graph === graph)
      return previous.view;
    const view = { ...card, graph };
    retainedCards[card.key] = { source: card, view };
    return view;
  }
  const cards = $derived(
    (store.phase !== "complete" && store.phase !== "error"
      ? planned.map((key) => byStage[key])
      : settled
    ).map(withGraph),
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
    if (key !== "bidirectional") return [moved, ...liveShape(key, true)];
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
      ...liveShape(key, false),
    ];
  }
  // Peak and Stability so far, read from the stage's series by the second, as the settled figures are: the
  // card fills in while the stage runs and the result replaces the estimate.
  function liveShape(key: Transfer, withPeak: boolean): SummaryRow[] {
    const graph = graphs[key];
    if (!graph) return [];
    const perLane = graph.lanes.map((lane) => {
      const seconds = new Map<number, { sum: number; n: number }>();
      for (const { t, v } of lane) {
        const second = Math.floor((t - graph.start) / 1000);
        const cell = seconds.get(second) ?? { sum: 0, n: 0 };
        cell.sum += v;
        cell.n++;
        seconds.set(second, cell);
      }
      return seconds;
    });
    const seconds = [...new Set(perLane.flatMap((lane) => [...lane.keys()]))];
    const rates = seconds
      .sort((a, b) => a - b)
      .map((second) =>
        perLane.reduce((total, lane) => {
          const cell = lane.get(second);
          return total + (cell ? cell.sum / cell.n : 0);
        }, 0),
      );
    if (!rates.length) return [];
    return [
      ...(withPeak
        ? [{ label: "Peak", value: formatRate(Math.max(...rates), units) }]
        : []),
      ...(rates.length >= 2
        ? [
            {
              label: "Stability",
              value: `${Math.round(stabilityPct(rates))}%`,
            },
          ]
        : []),
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
  out={view.out}
  details={details ?? store.serverDetails}
  issues={view.shown.issues}
  scope={details ? shown : ""}
  running={store.isRunning}
/>
