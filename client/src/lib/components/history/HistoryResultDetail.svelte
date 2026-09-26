<script module lang="ts">
  export interface ServerView {
    recordId: string;
    resultId: string;
    latencyId?: string;
  }
</script>

<script lang="ts">
  import { historyWirePresentation } from "../../history/wire";
  import { httpProtocolLabel } from "../../runner/protocol";
  import { serverLabel } from "../../presentation/serverAppearance";
  import { historyServers } from "../../history/servers";
  import { tooltip } from "../../actions/tooltip";
  import { bidirectionalResultPresentation } from "../../presentation/bidirectionalResult";
  import { ICON } from "../../constants";
  import {
    formatHistoryRate,
    formatLatency,
    formatPercent,
  } from "../../history/format";
  import type { HistoryRecord, ThroughputSnapshot } from "../../history/types";
  import { fmtBytes, fmtDuration } from "../../format";
  import { latencyLanes } from "../../runner/latencySummary";
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
  import ServerSelector from "../ServerSelector.svelte";
  import DiagnosticDetails from "../DiagnosticDetails.svelte";
  import LatencyProfileView from "../LatencyProfileView.svelte";

  interface Props {
    record: HistoryRecord;
    onClose: () => void;
    onDelete: () => void;
    region?: HTMLElement;
    closeButton?: HTMLButtonElement;
    serverView?: ServerView | null;
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
    serverView = $bindable(null),
  }: Props = $props();
  const view = $derived(serverView?.recordId === record.id ? serverView : null);
  const resultId = $derived(view?.resultId ?? "");
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
    serverView = {
      recordId: record.id,
      resultId: id,
      latencyId: view?.latencyId,
    };
  }
  const focusedId = $derived(
    view?.latencyId ?? record.multiServer?.latencyFocus,
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
      ? latencyLanes(focused.latency, focused.latencyByStage)
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
    return fmtBytes(result.totalBytes, store.unitBase);
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
              // Records with server details project medians; older lanes saved means.
              centerKind:
                focused || meta.key === "latency"
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

  function path(
    kind: string | null | undefined,
    protocol?: string | null,
    browserProtocol?: string,
  ): string {
    const mechanism =
      kind === "fetch-stream"
        ? "Fetch stream"
        : kind === "webtransport"
          ? "WebTransport"
          : kind === "webtransport-datagram"
            ? "WebTransport datagrams"
            : kind === "websocket"
              ? "WebSocket"
              : "Not recorded";
    const observed = browserProtocol
      ? httpProtocolLabel(browserProtocol)
      : null;
    const endpoint = protocol ? httpProtocolLabel(protocol) : null;
    return [
      mechanism,
      observed ?? endpoint,
      observed && endpoint && observed !== endpoint && protocol !== "negotiated"
        ? `endpoint ${endpoint}`
        : null,
    ]
      .filter(Boolean)
      .join(" · ");
  }
  const latencyKind = $derived(
    focused ? focused.latencyTarget?.transport : record.transport.latency.kind,
  );
  const latencySource = $derived(
    focused ? serverLabel(focused.server) : record.server.name,
  );
  const latencyPath = $derived(
    path(latencyKind, focused ? null : record.transport.latency.protocol),
  );
  const multiple = $derived((record.multiServer?.selection.length ?? 0) > 1);

  const contextRows = $derived(
    [
      ...(!multiple
        ? [
            {
              label: "Throughput path",
              value: path(
                record.transport.throughput.kind,
                record.transport.throughput.protocol,
              ),
            },
            { label: "Latency path", value: latencyPath },
          ]
        : []),
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
  const servers = $derived(historyServers(record));
</script>

<article
  bind:this={region}
  class="result-detail"
  aria-labelledby={`result-${record.id}-title`}
  tabindex="-1"
>
  <header class="surface-head detail-hero">
    <div class="detail-toolbar">
      <div class="detail-title">
        <span class="caps">Saved result</span>
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
        class="btn btn-icon btn-inset close-detail"
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
        <dd>{fmtDuration(record.durationMs)}</dd>
      </div>
      <div>
        <dt>Transferred</dt>
        <dd>{fmtBytes(record.totalBytes, store.unitBase)}</dd>
      </div>
    </dl>
  </header>

  {#if record.multiServer && record.multiServer.selection.length > 1}<div
      class="saved-server-context"
    >
      <span class="scope-label">Throughput results</span>
      <ResultServerContext
        details={record.multiServer}
        value={resultId}
        onchange={selectResult}
      />
      {#if scoped}<p class="selected-path">
          {path(
            scoped.throughput.transport,
            scoped.throughput.protocol,
            scoped.throughput.browserProtocol,
          )} <span>{scoped.throughput.origin}</span>
        </p>{/if}
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
          <article class="surface throughput-card" data-tone={card.tone}>
            <header>
              <span class="tone-icon" aria-hidden="true">{@html card.icon}</span
              >
              <strong>{card.label}</strong>
            </header>
            <p>{card.value}</p>
            {#if !scoped && store.showWireEstimates}
              {@const wire = historyWirePresentation(record, card.key)}
              {#if wire}<div class="saved-wire">
                  <span>{rate(wire.bytesPerSec)}</span><span
                    class="term"
                    use:tooltip={wire.tooltip}
                    >wire{wire.pct ? ` ${wire.pct}` : ""}</span
                  >
                </div>{/if}
            {/if}
            <small>{card.detail}</small>
          </article>
        {/each}
      </div>
    </section>
  {:else if scoped}<p class="missing-server-throughput">
      No throughput measurements available for this server.
    </p>{/if}

  {#if latencyProfiles.length || hasServerLatency || probeTimeoutLanes.length}
    <section
      class="detail-section"
      aria-labelledby={`result-${record.id}-latency`}
    >
      <header class="section-head">
        <span aria-hidden="true">{@html ICON.ping}</span>
        <h3 id={`result-${record.id}-latency`}>Responsiveness</h3>
      </header>
      <div class="section-body responsiveness-body">
        {#if record.multiServer && multiple}
          <div class="server-focus">
            <span>Latency source</span>
            <ServerSelector
              servers={record.multiServer.selection}
              value={focusedId ?? ""}
              label="Saved latency source"
              onchange={(id) => {
                serverView = { recordId: record.id, resultId, latencyId: id };
              }}
            />
          </div>
        {/if}
        {#if multiple}<p class="latency-path">
            {latencyPath}{#if focused?.latencyTarget}<span
                >{focused.latencyTarget.origin}</span
              >{/if}
          </p>{/if}
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
        {#if probeTimeoutLanes.length}<DiagnosticDetails label="Probe details"
            ><div class="probe-timeouts-section">
              <div class="probe-intro">
                <strong>{latencySource}</strong>
                <span
                  >{latencyKind === "webtransport"
                    ? "Datagram probe outcomes"
                    : latencyKind === "websocket"
                      ? "WebSocket probe outcomes"
                      : "Probe outcomes"}</span
                >
                <small
                  >Timeouts: no reply before the deadline. Unfinished probes and
                  failed sends are counted separately.</small
                >
              </div>
              <ul class="probe-timeouts-lanes">
                {#each probeTimeoutLanes as lane (lane.key)}
                  <li
                    data-tone={lane.tone}
                    aria-label={`${lane.label} probe timeouts ${lane.value}, ${lane.details}`}
                  >
                    <span class="tone-icon" aria-hidden="true"
                      >{@html lane.icon}</span
                    >
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
            </div></DiagnosticDetails
          >{/if}
      </div>
    </section>
  {/if}

  {#if record.failures.length}
    <div class="detail-section diagnostic-actions">
      {#if record.failures.length}<DiagnosticDetails label="Stage issues">
          <ul class="issue-list">
            {#each record.failures as failure}
              <li class="notice" data-tone="warn">
                <strong
                  >{failure.stage}{failure.direction
                    ? ` ${failure.direction}`
                    : ""}</strong
                >
                <span>{failure.reason.replaceAll("-", " ")}</span>
              </li>
            {/each}
          </ul></DiagnosticDetails
        >{/if}
    </div>
  {/if}

  <details class="disclosure run-metadata">
    <summary>Servers &amp; run context</summary>
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

    <section
      class="detail-section saved-servers-section"
      aria-labelledby={`result-${record.id}-servers`}
    >
      <header class="section-head">
        <span aria-hidden="true">{@html ICON.server}</span>
        <h3 id={`result-${record.id}-servers`}>Servers</h3>
      </header>
      <ul class="section-body saved-servers">
        {#each servers as server (server.id)}
          {@const measurement = record.multiServer?.servers.find(
            (entry) => entry.server.id === server.id,
          )}
          <li>
            <div>
              <strong>{server.label}</strong>
              {#if server.host}<small>{server.host}</small>{/if}
            </div>
            {#if server.ping}<span class="saved-server-ping"
                >Latency source</span
              >{/if}
            {#if multiple}
              <dl class="server-results" aria-label={`${server.label} results`}>
                <div>
                  <dt>Down</dt>
                  <dd>{rate(measurement?.download?.reportedBytesPerSec)}</dd>
                </div>
                <div>
                  <dt>Up</dt>
                  <dd>{rate(measurement?.upload?.reportedBytesPerSec)}</dd>
                </div>
                <div>
                  <dt>Idle</dt>
                  <dd>
                    {formatLatency(measurement?.latency?.reportedMs ?? null)}
                  </dd>
                </div>
              </dl>
              <dl class="server-paths">
                <div>
                  <dt>Throughput</dt>
                  <dd>
                    {measurement
                      ? path(
                          measurement.throughput.transport,
                          measurement.throughput.protocol,
                          measurement.throughput.browserProtocol,
                        )
                      : "Not measured"}{#if measurement}<small
                        >{measurement.throughput.origin}</small
                      >{/if}
                  </dd>
                </div>
                <div>
                  <dt>Latency</dt>
                  <dd>
                    {measurement?.latencyTarget
                      ? path(measurement.latencyTarget.transport)
                      : "Not measured"}{#if measurement?.latencyTarget}<small
                        >{measurement.latencyTarget.origin}</small
                      >{/if}
                  </dd>
                </div>
              </dl>
            {/if}
          </li>
        {/each}
      </ul>
    </section>
  </details>

  <footer class="detail-actions">
    <button class="btn btn-danger" type="button" onclick={onDelete}
      >Delete this result</button
    >
  </footer>
</article>

<style>
  .result-detail {
    min-width: 0;
    background: var(--surface-1);
    container: detail / inline-size;
  }
  .result-detail:focus {
    outline: none;
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
  .detail-title > .caps {
    color: var(--brand-strong);
  }
  h2 {
    margin-top: var(--space-1);
    font: var(--w-strong) clamp(18px, 2vw, 23px) var(--font-display);
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
  dt {
    color: var(--text-muted);
    font-size: var(--type-2xs);
  }
  dd {
    margin-top: 3px;
    overflow-wrap: anywhere;
    font: var(--w-strong) var(--type-xs) var(--font-mono);
  }
  .run-facts,
  .idle-summary {
    display: grid;
    grid-template-columns: repeat(var(--facts, 3), minmax(0, 1fr));
  }
  .run-facts {
    border-top: 1px solid var(--border);
  }
  .run-facts div {
    min-width: 0;
    padding: 10px var(--space-4) var(--space-3);
  }
  .run-facts > div + div,
  .idle-summary > div + div {
    border-left: 1px solid var(--border-subtle);
  }
  .run-facts dd.partial {
    color: var(--warn);
  }
  .detail-section,
  .saved-server-context,
  .run-metadata > summary {
    border-bottom: 1px solid var(--border);
  }
  .section-head {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-3) var(--space-4);
    background: var(--sheen);
  }
  .section-head > span {
    display: grid;
    flex: none;
    place-items: center;
    width: 20px;
    height: 20px;
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
  .section-body {
    padding: 0 var(--space-4) var(--space-4);
  }
  .throughput-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 150px), 1fr));
    gap: var(--space-2);
  }
  .throughput-card {
    display: grid;
    align-content: start;
    gap: 6px;
    min-width: 0;
    padding: var(--space-2);
  }
  .throughput-card[data-tone="bidirectional"] {
    grid-column: 1 / -1;
  }
  .throughput-card header {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }
  .throughput-card header strong {
    min-width: 0;
    font-size: var(--type-sm);
    font-weight: 700;
  }
  .throughput-card p {
    overflow-wrap: anywhere;
    font: 600 clamp(15px, 1.7vw, 18px) / 1.1 var(--font-display);
    font-variant-numeric: tabular-nums;
    letter-spacing: var(--track-tight);
  }
  .throughput-card small {
    overflow-wrap: anywhere;
    color: var(--text-muted);
    font: var(--w-normal) var(--type-2xs) / 1.4 var(--font-mono);
  }
  .saved-wire {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 6px;
    color: var(--brand-strong);
    font: 600 var(--type-xs) / 1.4 var(--font-mono);
    font-variant-numeric: tabular-nums;
  }
  .saved-wire .term {
    color: var(--text-soft);
    font-size: var(--type-2xs);
    font-weight: var(--w-normal);
  }
  .responsiveness-body {
    display: grid;
    gap: var(--space-3);
  }
  .responsiveness-body :global(.details-trigger) {
    justify-self: start;
  }
  .idle-summary {
    --facts: 4;
    padding: 10px var(--space-3);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-well);
    background: var(--sheen), var(--surface-1);
    box-shadow: var(--elev-tile);
  }
  .idle-summary div {
    min-width: 0;
  }
  .idle-summary > div + div {
    padding-left: var(--space-3);
  }
  .server-focus {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
    color: var(--text-soft);
    font-size: var(--type-sm);
  }
  .selected-path,
  .latency-path {
    color: var(--text-muted);
    font: var(--type-xs) / 1.5 var(--font-sans);
    overflow-wrap: anywhere;
  }
  .selected-path {
    padding-top: var(--space-2);
  }
  .selected-path span,
  .latency-path span {
    display: block;
    color: var(--text-soft);
  }
  .saved-server-context,
  .missing-server-throughput {
    padding: var(--space-3) var(--space-4);
  }
  .missing-server-throughput,
  .latency-empty {
    color: var(--text-muted);
    font-size: var(--type-sm);
  }
  .latency-empty {
    padding-block: var(--space-3);
  }
  .scope-label {
    display: block;
    margin-bottom: var(--space-2);
    color: var(--text-muted);
    font-size: var(--type-xs);
  }
  .saved-server-context :global(.scope-heading) {
    justify-content: flex-start;
  }
  .diagnostic-actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
  }
  .probe-intro {
    display: grid;
    gap: 3px;
    margin-bottom: var(--space-3);
  }
  .probe-intro strong {
    font-size: var(--type-sm);
  }
  .probe-intro span,
  .probe-intro small,
  .probe-timeouts-lanes small,
  .probe-exceptions {
    color: var(--text-muted);
    font: var(--w-normal) var(--type-xs) / 1.4 var(--font-sans);
    font-variant-numeric: tabular-nums;
  }
  .probe-intro small {
    margin-top: 5px;
  }
  .probe-timeouts-lanes {
    display: grid;
    gap: var(--space-2);
  }
  .probe-timeouts-lanes li {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
    padding-block: var(--space-2);
    border-top: 1px solid var(--border-subtle);
  }
  .probe-timeouts-lanes li > span:not(.tone-icon) {
    display: grid;
    min-width: 0;
  }
  .probe-timeouts-lanes strong {
    overflow: hidden;
    font-size: var(--type-xs);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .probe-timeouts-lanes .reply-count {
    margin-top: 3px;
  }
  .probe-exceptions {
    grid-column: 1 / -1;
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-1) var(--space-2);
  }
  .probe-timeouts-lanes em {
    color: var(--tone);
    font: 700 var(--type-sm) var(--font-display);
    font-variant-numeric: tabular-nums;
    font-style: normal;
  }
  .issue-list {
    display: grid;
    gap: 6px;
  }
  .issue-list li {
    justify-content: space-between;
    text-transform: capitalize;
  }
  .issue-list span {
    color: var(--text-muted);
    text-align: end;
  }
  .run-metadata > summary {
    padding: var(--space-3) var(--space-4);
  }
  .context-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 200px), 1fr));
    gap: 0 var(--space-4);
  }
  .context-grid > div {
    min-width: 0;
    padding-block: 9px;
    border-bottom: 1px solid var(--border-subtle);
  }
  .context-grid dd {
    font-size: var(--type-sm);
    line-height: 1.4;
  }
  .saved-servers li {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    align-items: baseline;
    gap: var(--space-3);
    padding-block: var(--space-2);
  }
  .saved-servers li + li {
    border-top: 1px solid var(--border);
  }
  .saved-servers strong {
    display: block;
    font-size: var(--type-sm);
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .saved-servers small {
    display: block;
    margin-top: 3px;
    color: var(--text-muted);
    font: var(--type-xs) / 1.4 var(--font-mono);
    overflow-wrap: anywhere;
  }
  .saved-server-ping {
    color: var(--text-muted);
    font-size: var(--type-xs);
  }
  .server-results,
  .server-paths {
    grid-column: 1 / -1;
    display: grid;
    gap: var(--space-2);
  }
  .server-results {
    grid-template-columns: repeat(3, minmax(0, 1fr));
  }
  .server-paths > div {
    display: grid;
    grid-template-columns: 70px minmax(0, 1fr);
    align-items: baseline;
    gap: var(--space-2);
  }
  .server-paths dd {
    margin: 0;
    font: var(--type-xs) / 1.5 var(--font-sans);
  }
  .detail-actions {
    display: flex;
    justify-content: flex-end;
    padding: var(--space-4);
  }
  @container detail (max-width: 560px) {
    .detail-toolbar,
    .detail-actions {
      padding: var(--space-3);
    }
    .run-facts div,
    .section-head {
      padding-inline: var(--space-3);
    }
    .section-body {
      padding: 0 var(--space-3) var(--space-3);
    }
    .idle-summary {
      --facts: 2;
      gap: var(--space-2) 0;
    }
    .idle-summary > div:nth-child(3) {
      padding-left: 0;
      border-left: 0;
    }
  }
</style>
