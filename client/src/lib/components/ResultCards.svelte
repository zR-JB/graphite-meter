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
  import { untrack } from "svelte";
  import {
    CARD_ORDER,
    pendingRows,
    resultSentence,
    summaryCards,
    serverIssues,
    summaryEvidence,
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
  // Until a run completes, every planned stage holds a card with all its rows; values fill in, nothing moves.
  const planned = $derived(
    CARD_ORDER.filter((key) => status(key) !== "disabled"),
  );
  const multiple = $derived(
    ((details ?? store.serverDetails)?.selection.length ??
      store.selectedServers.length) > 1,
  );
  const skeleton = (key: Stage) =>
    pendingRows(
      key,
      planned.filter((stage) => stage !== "latency"),
      multiple,
    );
  const same = (a: SummaryRow, b: SummaryRow) =>
    a.label === b.label && a.stage === b.stage;
  function held(card: SummaryCard): SummaryCard {
    const rows = skeleton(card.key);
    return {
      ...card,
      rows: [
        ...rows.map((row) => card.rows.find((got) => same(row, got)) ?? row),
        ...card.rows.filter((got) => !rows.some((row) => same(row, got))),
      ],
    };
  }
  const cards = $derived(
    store.phase !== "complete" && store.phase !== "error"
      ? planned.map((key) => {
          const card = settled.find((card) => card.key === key);
          return card ? held(card) : liveCard(key);
        })
      : settled,
  );

  // Once per completed run, never again for a unit or scope change.
  $effect(() => {
    if (store.phase !== "complete" || store.result === spoken) return;
    spoken = store.result;
    announce(untrack(() => resultSentence(settled)));
  });

  // A running stage fills its facts as it goes: bytes so far, and each bidirectional lane's rate.
  function liveRows(key: Stage): SummaryRow[] {
    const lanes = { download: live.rates?.down, upload: live.rates?.up };
    return skeleton(key).map((row) =>
      row.label === "Transferred"
        ? { ...row, value: fmtBytes(store.liveStageBytes, units.base) }
        : key === "bidirectional" && row.stage && row.stage !== "latency"
          ? {
              ...row,
              value:
                lanes[row.stage as "download" | "upload"] == null
                  ? MISSING
                  : formatRate(
                      lanes[row.stage as "download" | "upload"],
                      units,
                    ),
            }
          : row,
    );
  }

  // Animated values are visual only; the accessible value uses receiver accounting.
  function liveCard(key: Stage): SummaryCard {
    const active = status(key) === "active" || status(key) === "recovering";
    const timeout = key === "latency" && active && store.liveLatencyLost;
    const { down = null, up = null } = store.live ?? {};
    const [value, accessible] = !active
      ? [null, null]
      : key === "latency"
        ? [(live.rtt.current ?? store.liveRtt) || null, store.liveRtt || null]
        : [
            live.rates ? live.rates.down + live.rates.up : null,
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
        : store.phase === "aborted"
          ? "stopped"
          : "pending",
      num: timeout ? MISSING : shown.num,
      unit: timeout ? "timeout" : shown.unit,
      tip: JARGON[key],
      rows: active ? liveRows(key) : skeleton(key),
      accessible: active
        ? timeout
          ? "probe timeout"
          : `${spoken.num} ${spoken.unit}`
        : undefined,
    };
  }
</script>

<ResultSummary
  {cards}
  details={details ?? store.serverDetails}
  issues={store.serverDetails ? serverIssues(store.serverDetails, shown) : []}
  locked={!details}
  scope={details ? shown : ""}
  onscope={selectScope}
/>
