<script lang="ts">
  import { tooltip } from "../../actions/tooltip";
  import { bidirectionalResultPresentation } from "../../presentation/bidirectionalResult";
  import { ICON } from "../../constants";
  import {
    formatDuration,
    formatHistoryBytes,
    formatHistoryRate,
    formatLatency,
    formatPercent,
  } from "../../history/format";
  import type { HistoryRecord, ThroughputSnapshot } from "../../history/types";
  import { historyLatencyLanes } from "../../history/types";
  import { store } from "../../state/store.svelte";
  import {
    LATENCY_LANES,
    savedLatencyHasProbeEvidence,
    PARTIAL_ACCOUNTING_HELP,
    probeAccountingDetails,
    probeAccountingSummary,
    hasProbeAccountingNotice,
    type LatencyProfileViewLane,
    type LatencyProfileTone,
  } from "../latencyProfile";
  import ResultServerContext from "../ResultServerContext.svelte";
  import ServerPills from "../ServerPills.svelte";
  import LatencyProfileView from "../LatencyProfileView.svelte";

  interface Props {
    record: HistoryRecord;
    onClose: () => void;
    onDelete: () => void;
    region?: HTMLElement;
    closeButton?: HTMLButtonElement;
  }

  interface ThroughputCard {
    key: "download" | "upload" | "bidirectional";
    label: string;
    icon: string;
    tone: LatencyProfileTone;
    value: string;
    detail: string;
  }

  let {
    record,
    onClose,
    onDelete,
    region = $bindable(),
    closeButton = $bindable(),
  }: Props = $props();
  let chosenServer = $state<{ recordId: string; serverId: string } | null>(
    null,
  );
  let resultChoice = $state<{ recordId: string; serverId: string } | null>(
    null,
  );
  const resultId = $derived(
    resultChoice?.recordId === record.id ? resultChoice.serverId : "",
  );
  const scoped = $derived(
    record.multiServer?.servers.find((server) => server.server.id === resultId),
  );
  const resultStages = $derived(
    scoped
      ? {
          download: scoped.download,
          upload: scoped.upload,
          bidirectional: scoped.bidirectional ?? { down: null, up: null },
        }
      : {
          download: record.stages.download.result,
          upload: record.stages.upload.result,
          bidirectional: record.stages.bidirectional,
        },
  );
  function selectResult(id: string) {
    resultChoice = { recordId: record.id, serverId: id };
    if (
      record.multiServer?.servers.some(
        (server) => server.server.id === id && server.latencyTarget,
      )
    )
      chosenServer = { recordId: record.id, serverId: id };
  }
  const focusedId = $derived(
    chosenServer?.recordId === record.id &&
      record.multiServer?.selection.some(
        (server) => server.id === chosenServer?.serverId,
      )
      ? chosenServer.serverId
      : record.multiServer?.latencyFocus,
  );
  const hasServerLatency = $derived(
    (record.multiServer?.selection.length ?? 0) > 1 &&
      record.multiServer?.servers.some(
        (server) =>
          server.latency !== null ||
          Object.values(server.latencyByStage).some((lane) => lane !== null),
      ),
  );
  const focused = $derived(
    record.multiServer?.servers.find(
      (server) => server.server.id === focusedId,
    ),
  );
  const focusedLatency = $derived(
    focused ? focused.latency : record.stages.latency.result,
  );
  const focusedLanes = $derived(
    focused
      ? historyLatencyLanes(focused.latency, focused.latencyByStage)
      : record.stages.latency.lanes,
  );
  const units = $derived({ base: store.unitBase, kind: store.unitKind });
  const completedDate = $derived(new Date(record.completedAt));
  const partial = $derived(
    (record.multiServer?.failures.length ?? 0) > 0 ||
      record.outcome === "incomplete" ||
      Object.values(record.stages.latency.lanes).some(
        (lane) => lane != null && !lane.accountingComplete,
      ) ||
      record.failures.length > 0 ||
      [
        record.stages.latency.status,
        record.stages.download.status,
        record.stages.upload.status,
        record.stages.bidirectional.status,
      ].some((status) => status === "partial" || status === "failed"),
  );
  function rate(value: number | null | undefined): string {
    return formatHistoryRate(value, units);
  }

  function bytes(result: Pick<ThroughputSnapshot, "totalBytes">): string {
    return formatHistoryBytes(result.totalBytes, store.unitBase);
  }

  function throughputCard(
    key: "download" | "upload",
    result: Pick<
      ThroughputSnapshot,
      "reportedBytesPerSec" | "totalBytes"
    > | null,
  ): ThroughputCard | null {
    if (!result) return null;
    return {
      key,
      label: key === "download" ? "Download" : "Upload",
      icon: key === "download" ? ICON.download : ICON.upload,
      tone: key,
      value: rate(result.reportedBytesPerSec),
      detail: `${bytes(result)} transferred`,
    };
  }

  function bidirectionalCard(): ThroughputCard | null {
    const stage = resultStages.bidirectional;
    if (!stage.down && !stage.up) return null;
    const model = bidirectionalResultPresentation(
      stage.down?.reportedBytesPerSec,
      stage.up?.reportedBytesPerSec,
    );
    const direction = model.survivingDirection;
    const directions = [
      stage.down ? `Down ${rate(stage.down.reportedBytesPerSec)}` : null,
      stage.up ? `Up ${rate(stage.up.reportedBytesPerSec)}` : null,
    ].filter((value): value is string => value !== null);
    return {
      key: "bidirectional",
      label: direction
        ? `Bidirectional ${direction === "down" ? "download" : "upload"}`
        : "Bidirectional",
      icon: ICON.bidirectional,
      tone: "bidirectional",
      value: rate(
        model.combinedBytesPerSec ?? (direction ? model[direction] : null),
      ),
      detail: direction
        ? "One lane available · combined result unavailable"
        : directions.join(" · "),
    };
  }

  const throughputCards = $derived<ThroughputCard[]>(
    [
      throughputCard("download", resultStages.download),
      throughputCard("upload", resultStages.upload),
      bidirectionalCard(),
    ].filter((card): card is ThroughputCard => card !== null),
  );

  function usefulLane(lane: LatencyProfileViewLane): boolean {
    return (
      hasProbeAccountingNotice(lane) ||
      [lane.min, lane.max, lane.p10, lane.p90, lane.center].some(
        (value) => value != null,
      )
    );
  }

  const savedLanes = $derived(
    LATENCY_LANES.flatMap((meta) => {
      const snapshot = focusedLanes[meta.key];
      return snapshot
        ? [
            {
              ...meta,
              tone: meta.key,
              icon: meta.key === "latency" ? ICON.ping : ICON[meta.key],
              ...snapshot,
              centerKind:
                meta.key === "latency"
                  ? ("result" as const)
                  : ("average" as const),
            },
          ]
        : [];
    }),
  );
  const latencyProfiles = $derived(savedLanes.filter(usefulLane));
  const probeTimeoutLanes = $derived(
    savedLatencyHasProbeEvidence(
      focused
        ? (focused.latencyTarget?.transport ?? null)
        : record.transport.latency.kind,
    )
      ? savedLanes
          .filter(
            (lane) =>
              lane.accountingComplete === false ||
              lane.count > 0 ||
              lane.unresolvedCount ||
              lane.sendFailureCount,
          )
          .map((lane) => ({
            ...lane,
            value:
              lane.accountingComplete === false
                ? "Partial"
                : formatPercent(
                    lane.timeoutRatio == null ? null : lane.timeoutRatio * 100,
                  ),
            details: probeAccountingDetails(lane),
            counts: probeAccountingSummary(lane),
          }))
      : [],
  );

  function transport(value: string | null): string | null {
    return value?.replaceAll("-", " ") ?? null;
  }

  const contextRows = $derived(
    [
      {
        label: "Server",
        value: `${record.server.name}${record.server.location ? ` · ${record.server.location}` : ""}`,
      },
      {
        label: "Throughput transport",
        value: transport(record.transport.throughput.kind),
      },
      {
        label: "Throughput protocol",
        value: record.transport.throughput.protocol,
      },
      {
        label: "Latency transport",
        value: transport(record.transport.latency.kind),
      },
      { label: "Latency protocol", value: record.transport.latency.protocol },
      {
        label: "IP family",
        value: record.ipVersion ? `IPv${record.ipVersion}` : null,
      },
      { label: "Client build", value: record.client.build },
      { label: "Server engine", value: record.server.engine },
    ].filter((row): row is { label: string; value: string } =>
      Boolean(row.value),
    ),
  );

  const wireRows = $derived(
    record.wireEstimates
      ? [
          {
            label: "Download",
            value: record.wireEstimates.downloadBytesPerSec,
          },
          { label: "Upload", value: record.wireEstimates.uploadBytesPerSec },
          {
            label: "Bidirectional",
            value: record.wireEstimates.bidirectionalBytesPerSec,
          },
        ].filter(
          (row): row is { label: string; value: number } => row.value != null,
        )
      : [],
  );
</script>

<article
  bind:this={region}
  class="result-detail"
  aria-labelledby={`result-${record.id}-title`}
  tabindex="-1"
>
  <header class="detail-hero">
    <div class="detail-toolbar">
      <div class="detail-title">
        <span>Saved result</span>
        <h2 id={`result-${record.id}-title`}>
          <time datetime={completedDate.toISOString()}>
            {completedDate.toLocaleDateString(undefined, {
              weekday: "short",
              month: "short",
              day: "numeric",
              year: "numeric",
            })}
            <em
              >{completedDate.toLocaleTimeString(undefined, {
                hour: "2-digit",
                minute: "2-digit",
              })}</em
            >
          </time>
        </h2>
      </div>
      <button
        bind:this={closeButton}
        class="close-detail"
        type="button"
        aria-label="Close result"
        onclick={onClose}
      >
        {@html ICON.close}
      </button>
    </div>
    <dl class="run-facts" aria-label="Run summary">
      <div>
        <dt>Completion</dt>
        <dd class:partial>{partial ? "Partial" : "Complete"}</dd>
      </div>
      <div>
        <dt>Actual duration</dt>
        <dd>{formatDuration(record.durationMs)}</dd>
      </div>
      <div>
        <dt>Transferred</dt>
        <dd>{formatHistoryBytes(record.totalBytes, store.unitBase)}</dd>
      </div>
    </dl>
  </header>

  {#if record.multiServer && record.multiServer.selection.length > 1}<div
      class="saved-server-context"
    >
      <ResultServerContext
        details={record.multiServer}
        value={resultId}
        onchange={selectResult}
      />
    </div>{/if}
  {#if throughputCards.length}
    <section
      class="detail-section"
      aria-labelledby={`result-${record.id}-throughput`}
    >
      <header class="section-head">
        <span aria-hidden="true">{@html ICON.bidirectional}</span>
        <h3 id={`result-${record.id}-throughput`}>Throughput</h3>
      </header>
      <div class="section-body throughput-grid">
        {#each throughputCards as card (card.key)}
          <article class="throughput-card" data-tone={card.tone}>
            <header>
              <span class="phase-icon" aria-hidden="true"
                >{@html card.icon}</span
              >
              <strong>{card.label}</strong>
            </header>
            <p>{card.value}</p>
            <small>{card.detail}</small>
          </article>
        {/each}
      </div>
    </section>
  {:else if scoped}<p class="missing-server-throughput">
      No throughput measurements available for this server.
    </p>{/if}

  {#if latencyProfiles.length || hasServerLatency}
    <section
      class="detail-section"
      aria-labelledby={`result-${record.id}-latency`}
    >
      <header class="section-head">
        <span aria-hidden="true">{@html ICON.ping}</span>
        <h3 id={`result-${record.id}-latency`}>Responsiveness</h3>
      </header>
      <div class="section-body responsiveness-body">
        {#if record.multiServer && record.multiServer.selection.length > 1}
          <div class="server-focus">
            <span
              >Latency to <strong
                >{focused?.server.name ?? "selected server"}</strong
              ></span
            >
            {#if record.multiServer.servers.filter((server) => server.latencyTarget).length > 1}
              <ServerPills
                servers={record.multiServer.selection}
                value={focusedId ?? ""}
                label="Saved latency server"
                disabledIds={record.multiServer.servers
                  .filter((server) => !server.latencyTarget)
                  .map((server) => server.server.id)}
                onchange={(id) =>
                  (chosenServer = { recordId: record.id, serverId: id })}
              />
            {/if}
          </div>
        {/if}
        {#if focusedLatency}
          <dl class="idle-summary" aria-label="Idle latency result">
            <div>
              <dt>Idle result</dt>
              <dd>{formatLatency(focusedLatency.reportedMs)}</dd>
            </div>
            <div>
              <dt>Median (p50)</dt>
              <dd>{formatLatency(focusedLatency.p50Ms)}</dd>
            </div>
            <div>
              <dt>p95</dt>
              <dd>{formatLatency(focusedLatency.p95Ms)}</dd>
            </div>
            <div>
              <dt>Stability</dt>
              <dd>
                {formatPercent(focusedLatency.stabilityScore * 100, 0)}
              </dd>
            </div>
          </dl>
        {/if}
        {#if latencyProfiles.length}
          <LatencyProfileView
            lanes={latencyProfiles}
            variant="compact"
            label="Saved latency distributions"
          />
        {:else if !focusedLatency}
          <p class="latency-empty">
            No latency measurements available for this server.
          </p>
        {/if}
      </div>
    </section>
  {/if}

  {#if probeTimeoutLanes.length}
    <section
      class="detail-section probe-timeouts-section"
      aria-labelledby={`result-${record.id}-probe-timeouts`}
    >
      <header class="section-head">
        <span aria-hidden="true">{@html ICON.ping}</span>
        <h3 id={`result-${record.id}-probe-timeouts`}>
          Probe timeouts ({record.transport.latency.kind === "webtransport"
            ? "datagram"
            : "WebSocket"})
        </h3>
        <span
          class="section-help"
          role="note"
          aria-label="About probe timeouts"
          use:tooltip={"No reply before the deadline. Application timeouts, not IP packet loss; unresolved probes and failed sends are separate."}
          >{@html ICON.info}</span
        >
      </header>
      <ul class="section-body probe-timeouts-lanes">
        {#each probeTimeoutLanes as lane (lane.key)}
          <li
            data-tone={lane.tone}
            aria-label={`${lane.label} probe timeouts ${lane.value}, ${lane.details}`}
          >
            <span class="phase-icon" aria-hidden="true">{@html lane.icon}</span>
            <span>
              <strong>{lane.label}</strong>
              <small class="reply-count">{lane.counts.replies}</small>
              {#if lane.accountingComplete === false}
                <small role="note" use:tooltip={PARTIAL_ACCOUNTING_HELP}
                  >Partial accounting</small
                >
              {/if}
            </span>
            <em>{lane.value}</em>
            {#if lane.counts.exceptions.length}
              <div class="probe-exceptions">
                {#each lane.counts.exceptions as detail}
                  <span>{detail}</span>
                {/each}
              </div>
            {/if}
          </li>
        {/each}
      </ul>
    </section>
  {/if}

  {#if contextRows.length}
    <section
      class="detail-section"
      aria-labelledby={`result-${record.id}-context`}
    >
      <header class="section-head">
        <span aria-hidden="true">{@html ICON.info}</span>
        <h3 id={`result-${record.id}-context`}>Run context</h3>
      </header>
      <dl class="section-body context-grid">
        {#each contextRows as row (row.label)}
          <div>
            <dt>{row.label}</dt>
            <dd>{row.value}</dd>
          </div>
        {/each}
      </dl>
    </section>
  {/if}

  {#if record.failures.length}
    <section
      class="detail-section"
      aria-labelledby={`result-${record.id}-issues`}
    >
      <header class="section-head">
        <span class="issue-icon" aria-hidden="true">!</span>
        <h3 id={`result-${record.id}-issues`}>Stage issues</h3>
      </header>
      <ul class="section-body issue-list">
        {#each record.failures as failure}
          <li>
            <strong
              >{failure.stage}{failure.direction
                ? ` ${failure.direction}`
                : ""}</strong
            >
            <span>{failure.reason.replaceAll("-", " ")}</span>
          </li>
        {/each}
      </ul>
    </section>
  {/if}

  {#if wireRows.length && store.showWireEstimates}
    <section
      class="detail-section"
      aria-labelledby={`result-${record.id}-wire`}
    >
      <header class="section-head">
        <span class="wire-icon" aria-hidden="true">W</span>
        <h3 id={`result-${record.id}-wire`}>Wire-rate snapshot</h3>
      </header>
      <dl class="section-body wire-grid">
        {#each wireRows as row (row.label)}
          <div>
            <dt>{row.label}</dt>
            <dd>{rate(row.value)}</dd>
          </div>
        {/each}
      </dl>
    </section>
  {/if}

  <footer class="detail-actions">
    <button type="button" onclick={onDelete}>Delete this result</button>
  </footer>
</article>

<style>
  .saved-server-context {
    padding: var(--space-3) var(--space-4);
    border-bottom: 1px solid var(--border);
  }
  .missing-server-throughput {
    padding: var(--space-3) var(--space-4);
    color: var(--text-muted);
  }
  .latency-empty {
    color: var(--text-muted);
    font-size: var(--type-sm);
    padding-block: var(--space-3);
  }
  .server-focus {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--text-soft);
    font-size: 12px;
  }

  .result-detail {
    min-width: 0;
    background: var(--surface-1);
    color: var(--text);
  }
  .result-detail:focus {
    outline: none;
  }
  h2,
  h3,
  p {
    margin: 0;
  }
  .detail-hero {
    border-bottom: 1px solid var(--border-strong);
    background:
      linear-gradient(180deg, var(--surface-2), var(--surface-1) 72%),
      var(--surface-1);
    box-shadow: var(--elev-tile);
  }
  .detail-toolbar {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--space-3);
    padding: var(--space-4);
  }
  .detail-title {
    min-width: 0;
  }
  .detail-title > span {
    color: var(--brand-strong);
    font: 800 9px var(--font-mono);
    letter-spacing: var(--track-wide);
    text-transform: uppercase;
  }
  h2 {
    margin-top: 4px;
    font: 650 clamp(18px, 2vw, 23px) var(--font-display);
    letter-spacing: var(--track-tight);
  }
  h2 time {
    display: grid;
    gap: 2px;
  }
  h2 em {
    color: var(--text-muted);
    font: 550 var(--type-sm) var(--font-mono);
    font-style: normal;
  }
  .close-detail {
    display: grid;
    place-items: center;
    width: 34px;
    height: 34px;
    flex: none;
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-inset);
    box-shadow: var(--elev-tile);
    color: var(--text-muted);
    cursor: pointer;
    transition:
      border-color var(--dur-hover) var(--ease-out),
      background var(--dur-hover) var(--ease-out),
      color var(--dur-hover) var(--ease-out);
  }
  .close-detail:hover {
    border-color: var(--border-strong);
    background: var(--surface-2);
    color: var(--text);
  }
  .close-detail :global(svg) {
    width: 17px;
    height: 17px;
  }
  .run-facts {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    margin: 0;
    border-top: 1px solid var(--border);
  }
  .run-facts div {
    min-width: 0;
    padding: 10px var(--space-4) 12px;
  }
  .run-facts div + div {
    border-left: 1px solid var(--border-subtle);
  }
  dt {
    color: var(--text-muted);
    font-size: 9px;
    letter-spacing: 0.03em;
  }
  dd {
    margin: 3px 0 0;
    overflow-wrap: anywhere;
    color: var(--text);
    font: 650 var(--type-xs) var(--font-mono);
  }
  .run-facts dd.partial {
    color: var(--warn);
  }
  .detail-section {
    border-bottom: 1px solid var(--border);
  }
  .section-head {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: 12px var(--space-4);
    background: linear-gradient(180deg, var(--surface-2), transparent);
  }
  .section-head > span {
    display: grid;
    place-items: center;
    width: 20px;
    height: 20px;
    flex: none;
    color: var(--brand-strong);
  }
  .section-head :global(svg) {
    width: 14px;
    height: 14px;
  }
  .section-head h3 {
    font-size: var(--type-sm);
    font-weight: 760;
    letter-spacing: -0.01em;
  }
  .section-head > .section-help {
    display: grid;
    place-items: center;
    width: 22px;
    height: 22px;
    margin-left: -4px;
    border-radius: 50%;
    color: var(--text-muted);
    cursor: help;
    transition:
      background var(--dur-hover) var(--ease-out),
      color var(--dur-hover) var(--ease-out);
  }
  .section-help:hover,
  .section-help:focus-visible {
    background: var(--surface-inset);
    color: var(--text);
  }
  .section-help :global(svg) {
    width: 13px;
    height: 13px;
  }
  .section-body {
    margin: 0;
    padding: 0 var(--space-4) var(--space-4);
  }
  .throughput-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
    gap: var(--space-2);
  }
  .throughput-card {
    --tone: var(--phase-complete);
    display: grid;
    align-content: start;
    gap: 6px;
    min-width: 0;
    padding: var(--space-2);
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-1);
    box-shadow: var(--elev-tile);
  }
  [data-tone="download"] {
    --tone: var(--phase-download);
  }
  [data-tone="upload"] {
    --tone: var(--phase-upload);
  }
  [data-tone="bidirectional"] {
    --tone: var(--phase-bidirectional);
  }
  .throughput-card header {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }
  .phase-icon {
    display: grid;
    place-items: center;
    width: 22px;
    height: 22px;
    flex: none;
    border: 1px solid color-mix(in srgb, var(--tone) 34%, var(--border));
    border-radius: var(--r-well);
    background: var(--surface-2);
    color: var(--tone);
  }
  .phase-icon :global(svg) {
    width: 13px;
    height: 13px;
  }
  .throughput-card header strong {
    min-width: 0;
    color: var(--text);
    font-size: var(--type-sm);
    font-weight: 700;
  }
  .throughput-card p {
    overflow-wrap: anywhere;
    font: 600 clamp(15px, 1.7vw, 18px) var(--font-display);
    font-variant-numeric: tabular-nums;
    letter-spacing: var(--track-tight);
    line-height: 1.1;
  }
  .throughput-card small {
    overflow-wrap: anywhere;
    color: var(--text-muted);
    font: 500 10px var(--font-mono);
    line-height: 1.4;
  }
  .responsiveness-body {
    display: grid;
    gap: var(--space-3);
  }
  .probe-timeouts-lanes {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 180px), 1fr));
    gap: var(--space-2);
    list-style: none;
  }
  .probe-timeouts-lanes li {
    --tone: var(--phase-latency);
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
    padding: var(--space-3);
    border: 1px solid var(--border-subtle);
    border-top-color: color-mix(in srgb, var(--tone) 40%, var(--border));
    border-radius: var(--r-well);
    background: var(--surface-1);
  }
  .probe-timeouts-lanes li > span:not(.phase-icon) {
    display: grid;
    min-width: 0;
  }
  .probe-timeouts-lanes strong {
    overflow: hidden;
    font-size: var(--type-xs);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .probe-timeouts-lanes small {
    color: var(--text-muted);
    font: 500 var(--type-xs)/1.4 var(--font-sans);
    font-variant-numeric: tabular-nums;
  }
  .probe-timeouts-lanes .reply-count {
    margin-top: 3px;
    font-size: 11px;
  }
  .probe-exceptions {
    grid-column: 1 / -1;
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-1) var(--space-2);
    color: var(--warn);
    font: 500 var(--type-xs)/1.4 var(--font-sans);
    font-variant-numeric: tabular-nums;
  }
  .probe-timeouts-lanes em {
    color: var(--tone);
    font: 700 var(--type-sm) var(--font-display);
    font-variant-numeric: tabular-nums;
    font-style: normal;
  }
  .idle-summary {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    margin: 0;
    padding: 10px 12px;
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-well);
    background:
      linear-gradient(180deg, var(--surface-2), transparent), var(--surface-1);
    box-shadow: var(--elev-tile);
  }
  .idle-summary div {
    min-width: 0;
  }
  .idle-summary div + div {
    padding-left: var(--space-3);
    border-left: 1px solid var(--border-subtle);
  }
  .context-grid,
  .wire-grid {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 0 var(--space-4);
  }
  .context-grid > div,
  .wire-grid > div {
    min-width: 0;
    padding: 9px 0;
    border-bottom: 1px solid var(--border-subtle);
  }
  .context-grid dd,
  .wire-grid dd {
    font-size: var(--type-sm);
    line-height: 1.4;
  }
  .issue-icon {
    color: var(--warn) !important;
    font: 800 var(--type-sm) var(--font-mono);
  }
  .issue-list {
    display: grid;
    gap: 6px;
    list-style: none;
  }
  .issue-list li {
    display: flex;
    justify-content: space-between;
    gap: var(--space-3);
    padding: 9px 10px;
    border: 1px solid color-mix(in srgb, var(--warn) 30%, var(--border));
    border-radius: var(--r-well);
    background: var(--warn-soft);
    font-size: var(--type-xs);
    text-transform: capitalize;
  }
  .issue-list span {
    color: var(--text-muted);
    text-align: right;
  }
  .wire-icon {
    color: var(--text-muted) !important;
    font: 750 10px var(--font-mono);
  }
  .detail-actions {
    display: flex;
    justify-content: flex-end;
    padding: var(--space-4);
  }
  .detail-actions button {
    min-height: 32px;
    padding: 0 var(--space-3);
    border: 1px solid color-mix(in srgb, var(--err) 48%, var(--border));
    border-radius: var(--r-chrome);
    background: transparent;
    color: var(--text-muted);
    font-size: var(--type-sm);
    font-weight: 700;
    cursor: pointer;
  }
  .detail-actions button:hover {
    border-color: var(--err);
    color: var(--err);
  }
  @media (prefers-reduced-motion: no-preference) {
    .detail-section,
    .throughput-card {
      animation: detail-content-enter var(--dur-hover) var(--ease-out) both;
    }
    .throughput-card:nth-child(2) {
      animation-delay: 25ms;
    }
    .throughput-card:nth-child(3) {
      animation-delay: 50ms;
    }
    @keyframes detail-content-enter {
      from {
        transform: translateY(3px);
      }
    }
  }
  @media (max-width: 560px) {
    .detail-toolbar,
    .detail-actions {
      padding: var(--space-3);
    }
    .run-facts div {
      padding-inline: var(--space-3);
    }
    .section-head {
      padding-inline: var(--space-3);
    }
    .section-body {
      padding-inline: var(--space-3);
      padding-bottom: var(--space-3);
    }
    .throughput-grid,
    .context-grid,
    .wire-grid {
      grid-template-columns: 1fr;
    }
    .idle-summary {
      grid-template-columns: repeat(2, minmax(0, 1fr));
      gap: var(--space-2) 0;
    }
    .idle-summary div:nth-child(3) {
      padding-left: 0;
      border-left: 0;
    }
  }
</style>
