<script lang="ts">
  import ResultSummary from "./ResultSummary.svelte";
  import { getApplicationController } from "../runner/controllerContext";
  import { store } from "../state/store.svelte";
  import { fmtSpeed, fmtMs } from "../format";
  import { MISSING, STAGE } from "../presentation/vocabulary";
  import type { LiveReadout } from "../presentation/liveReadout.svelte";
  import {
    CARD_ORDER,
    summaryCards,
    summaryEvidence,
    type SummaryCard,
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
  const rate = (bytesPerSec: number) => ({
    num: fmtSpeed(store.toUnit(bytesPerSec)),
    unit: store.unitLabel,
  });
  const status = (key: Stage) => store.stagePresentation[key].status;

  // Every stage holds its card from the start; a settled stage fills in its values.
  const cards = $derived.by(() => {
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
    const settled = summaryCards(
      evidence,
      rate,
      store.unitBase,
      store.showWireEstimates,
    );
    if (!store.isRunning) return settled;
    return CARD_ORDER.flatMap((key) =>
      status(key) === "disabled"
        ? []
        : [settled.find((card) => card.key === key) ?? liveCard(key)],
    );
  });

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
    const format = (n: number | null) =>
      n === null ? MISSING : key === "latency" ? fmtMs(n) : rate(n).num;
    const unit =
      key === "latency" ? (timeout ? "timeout" : "ms") : store.unitLabel;
    return {
      key,
      label: STAGE[key].short,
      icon: STAGE[key].icon,
      status: active ? "active" : "pending",
      num: timeout ? MISSING : format(value),
      unit,
      rows: [],
      details: [],
      accessible: active
        ? `${timeout ? "probe timeout" : format(accessible)} ${unit}`
        : undefined,
    };
  }
</script>

<ResultSummary {cards} {details} scope={shown} onscope={selectScope} />
