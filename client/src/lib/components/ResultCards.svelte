<script module lang="ts">
  // Module scope: returning from History must not announce the same run again.
  let spoken: unknown = null;
</script>

<script lang="ts">
  import ResultSummary from "./ResultSummary.svelte";
  import { getApplicationController } from "../runner/controllerContext";
  import { store } from "../state/store.svelte";
  import { fmtBytes, fmtMs, formatRate, resultRate } from "../format";
  import { JARGON, MISSING, STAGE } from "../presentation/vocabulary";
  import type { LiveReadout } from "../presentation/liveReadout.svelte";
  import { announce } from "../presentation/announcer.svelte";
  import { handoff } from "../presentation/motion.svelte";
  import { untrack } from "svelte";
  import {
    CARD_ORDER,
    resultSentence,
    summaryCards,
    serverIssues,
    summaryEvidence,
    tracePaths,
    type SummaryCard,
    type SummaryRow,
  } from "../presentation/resultSummary";

  type Stage = (typeof CARD_ORDER)[number];

  let { live }: { live: LiveReadout } = $props();

  const controller = getApplicationController();
  let shown = $state("");
  const details = $derived(store.result?.multiServer);

  function selectScope(id: string) {
    shown = id;
    if (details?.servers.some((s) => s.server.id === id && s.latencyTarget))
      controller.focusServer(id);
  }
  const units = $derived({ base: store.unitBase, kind: store.unitKind });
  const status = (key: Stage) => store.stagePresentation[key].status;

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
      details?.latencyFocus,
    );
    return summaryCards(evidence, units, store.showWireEstimates);
  });
  // Until a run completes, every planned stage holds a card; values fill in, nothing moves.
  const planned = $derived(
    CARD_ORDER.filter((key) => status(key) !== "disabled"),
  );
  // The chart's series per stage, over its plan until complete; the run's, so a scoped server shows no throughput trace.
  const traces = $derived.by(() => {
    const plan = (store.run?.config ?? store.config).duration;
    const lane = (key: Stage, dir: "down" | "up") =>
      store.throughput
        .filter((s) => s.phase === key && s.dir === dir)
        .map((s) => ({ t: s.t, v: s.bytesPerSec }));
    const idle = store.latency.flatMap((b) =>
      b.phase === "latency" && b.medianRttMs !== null
        ? [{ t: b.t, v: b.medianRttMs }]
        : [],
    );
    const trace = (key: Stage, series: { t: number; v: number }[][]) =>
      tracePaths(series, status(key) === "complete" ? 0 : plan[`${key}Ms`]);
    return {
      latency: trace("latency", [idle]),
      ...(!shown && {
        download: trace("download", [lane("download", "down")]),
        upload: trace("upload", [lane("upload", "up")]),
        bidirectional: trace("bidirectional", [
          lane("bidirectional", "down"),
          lane("bidirectional", "up"),
        ]),
      }),
    } as Partial<Record<Stage, SummaryCard["trace"]>>;
  });
  const cards = $derived(
    (store.phase !== "complete" && store.phase !== "error"
      ? planned.map(
          (key) => settled.find((card) => card.key === key) ?? liveCard(key),
        )
      : settled
    ).map((card) => ({ ...card, trace: traces[card.key] })),
  );

  const view = handoff(
    () => ({
      run: store.runSeq,
      cards,
      issues: store.serverDetails
        ? serverIssues(store.serverDetails, shown)
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

  // A running stage fills its facts as it goes: bytes so far, and each bidirectional lane's rate.
  function liveRows(key: Stage): SummaryRow[] {
    if (key === "latency") return [];
    if (key !== "bidirectional")
      return [
        {
          label: "Transferred",
          value: fmtBytes(store.liveStageBytes, units.base),
        },
      ];
    return (["download", "upload"] as const).map((stage) => {
      const rate = live.rates?.[stage === "download" ? "down" : "up"];
      return {
        label: STAGE[stage].short,
        value: rate == null ? MISSING : formatRate(rate, units),
        stage,
      };
    });
  }

  // Animated values are visual only; the accessible value uses receiver accounting.
  function liveCard(key: Stage): SummaryCard {
    const active = status(key) === "active" || status(key) === "recovering";
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
        ? "active"
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
  fade={view.opacity}
  reserve
  details={details ?? store.serverDetails}
  issues={view.shown.issues}
  locked={!details}
  scope={details ? shown : ""}
  onscope={selectScope}
/>
