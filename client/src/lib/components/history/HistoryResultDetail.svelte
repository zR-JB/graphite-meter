<script lang="ts">
  import { historyWirePresentation } from "../../history/wire";
  import { httpProtocolLabel } from "../../runner/protocol";
  import { serverLabel } from "../../presentation/serverAppearance";
  import { tooltip } from "../../actions/tooltip";
  import { ICON } from "../../constants";
  import { fmtBytes, fmtDuration } from "../../format";
  import {
    formatHistoryRate,
    formatLatency,
    historyOutcome,
    historyRate,
  } from "../../history/format";
  import type { HistoryRecord } from "../../history/types";
  import { latencyLanes } from "../../runner/latencySummary";
  import { store } from "../../state/store.svelte";
  import { OUTCOME, STAGE, MISSING } from "../../presentation/vocabulary";
  import {
    serverEvidence,
    summaryCards,
    type SummaryEvidence,
  } from "../../presentation/resultSummary";
  import {
    LATENCY_LANES,
    savedLatencyHasProbeEvidence,
    PARTIAL_ACCOUNTING_HELP,
    probeAccountingDetails,
    probeAccountingSummary,
    hasProbeAccountingNotice,
  } from "../latencyProfile";
  import MoreMenu from "../MoreMenu.svelte";
  import ResultSummary from "../ResultSummary.svelte";
  import LatencyProfileView from "../LatencyProfileView.svelte";

  interface Props {
    record: HistoryRecord;
    onClose: () => void;
    onDelete: (invoker: HTMLElement) => void;
    region?: HTMLElement;
  }
  let { record, onClose, onDelete, region = $bindable() }: Props = $props();

  let shown = $state("");
  const details = $derived(
    (record.multiServer?.selection.length ?? 0) > 1 ? record.multiServer : null,
  );
  const units = $derived({ base: store.unitBase, kind: store.unitKind });
  const outcome = $derived(historyOutcome(record));
  const completed = $derived(new Date(record.completedAt));

  const cards = $derived.by(() => {
    const status: SummaryEvidence["status"] = {};
    for (const key of LATENCY_LANES.map((lane) => lane.key)) {
      const value = record.stages[key].status;
      if (value !== "not-run") status[key] = value;
    }
    const wire = (key: "download" | "upload" | "bidirectional") =>
      store.showWireEstimates ? historyWirePresentation(record, key) : null;
    const evidence =
      (details && shown && serverEvidence(details, shown, status)) ||
      ({
        status,
        download: record.stages.download.result,
        upload: record.stages.upload.result,
        bidirectional: record.stages.bidirectional,
        latency: record.stages.latency.result,
        latencyMeasured: true,
        latencySource: details?.selection.find(
          (server) => server.id === details.latencyFocus,
        )?.name,
        wire: {
          download: wire("download"),
          upload: wire("upload"),
          bidirectional: wire("bidirectional"),
        },
      } satisfies SummaryEvidence);
    return summaryCards(
      evidence,
      (value) => historyRate(value, units),
      store.unitBase,
    );
  });

  // Latency follows the chosen server when it measured latency, else the headline server.
  const latencyServer = $derived(
    details?.servers.find(
      (server) => server.server.id === shown && server.latencyTarget,
    ) ??
      details?.servers.find(
        (server) => server.server.id === details.latencyFocus,
      ),
  );
  const lanes = $derived.by(() => {
    const saved = latencyServer
      ? latencyLanes(latencyServer.latency, latencyServer.latencyByStage)
      : record.stages.latency.lanes;
    return LATENCY_LANES.flatMap((meta) => {
      const lane = saved[meta.key];
      return lane
        ? [
            {
              ...meta,
              ...lane,
              tone: meta.key,
              // Lanes saved with p95 or server details carry medians; older ones means.
              centerKind:
                latencyServer || "p95" in lane || meta.key === "latency"
                  ? ("result" as const)
                  : ("average" as const),
            },
          ]
        : [];
    });
  });
  const profile = $derived(
    lanes.filter(
      (lane) =>
        hasProbeAccountingNotice(lane) ||
        [lane.min, lane.max, lane.center].some((value) => value != null),
    ),
  );
  const accounting = $derived(
    savedLatencyHasProbeEvidence(
      latencyServer
        ? (latencyServer.latencyTarget?.transport ?? null)
        : record.transport.latency.kind,
    )
      ? lanes.filter(
          (lane) =>
            lane.accountingComplete === false ||
            lane.count > 0 ||
            lane.unresolvedCount ||
            lane.sendFailureCount,
        )
      : [],
  );

  function path(
    kind: string | null | undefined,
    protocol?: string | null,
    browserProtocol?: string,
  ): string {
    const mechanism =
      {
        "fetch-stream": "Fetch stream",
        webtransport: "WebTransport",
        "webtransport-datagram": "WebTransport datagrams",
        websocket: "WebSocket",
      }[kind ?? ""] ?? "Not recorded";
    const observed = browserProtocol && httpProtocolLabel(browserProtocol);
    const endpoint = protocol && httpProtocolLabel(protocol);
    return [
      mechanism,
      observed || endpoint,
      observed && endpoint && observed !== endpoint && protocol !== "negotiated"
        ? `endpoint ${endpoint}`
        : null,
    ]
      .filter(Boolean)
      .join(" · ");
  }
  const rate = (value: number | null | undefined) =>
    formatHistoryRate(value, units);
  const serverRows = $derived(
    details
      ? details.selection.map((server) => {
          const measured = details.servers.find(
            (entry) => entry.server.id === server.id,
          );
          return {
            id: server.id,
            name: serverLabel(server),
            host: new URL(server.url).host,
            down: rate(measured?.download?.reportedBytesPerSec),
            up: rate(measured?.upload?.reportedBytesPerSec),
            latency: formatLatency(measured?.latency?.reportedMs),
            throughputPath: measured
              ? path(
                  measured.throughput.transport,
                  measured.throughput.protocol,
                  measured.throughput.browserProtocol,
                )
              : "Not measured",
            latencyPath: measured?.latencyTarget
              ? path(measured.latencyTarget.transport)
              : "Not measured",
          };
        })
      : [
          {
            id: "single",
            name: serverLabel({
              name: record.server.name,
              location: record.server.location ?? undefined,
            }),
            host: "",
            down: rate(record.stages.download.result?.reportedBytesPerSec),
            up: rate(record.stages.upload.result?.reportedBytesPerSec),
            latency: formatLatency(record.stages.latency.result?.reportedMs),
            throughputPath: path(
              record.transport.throughput.kind,
              record.transport.throughput.protocol,
            ),
            latencyPath: path(
              record.transport.latency.kind,
              record.transport.latency.protocol,
            ),
          },
        ],
  );
  const issues = $derived([
    ...record.failures.map(
      (failure) =>
        `${STAGE[failure.stage].label}${failure.direction ? ` ${failure.direction}` : ""} · ${failure.reason.replaceAll("-", " ")}`,
    ),
    ...(record.multiServer?.failures ?? []).map(
      (failure) =>
        `${record.multiServer!.selection.find((server) => server.id === failure.serverId)?.name ?? "Server"} · ${STAGE[failure.stage].label}${failure.scope === "latency" ? " latency" : ""} · ${failure.message}`,
    ),
  ]);
  const environment = $derived(
    [
      ["IP family", record.ipVersion ? `IPv${record.ipVersion}` : null],
      ["Client build", record.client.build],
      ["Server engine", record.server.engine],
    ].filter((row): row is [string, string] => !!row[1]),
  );
</script>

<article
  bind:this={region}
  class="result-detail"
  aria-labelledby={`result-${record.id}-title`}
  tabindex="-1"
>
  <header class="surface-head detail-head">
    <button
      class="btn btn-quiet back"
      type="button"
      aria-label="Back to results"
      onclick={onClose}
    >
      <span aria-hidden="true">←</span>Results
    </button>
    <div class="title">
      <h2 id={`result-${record.id}-title`}>
        <time datetime={completed.toISOString()}
          >{completed.toLocaleString(undefined, {
            weekday: "short",
            month: "short",
            day: "numeric",
            year: "numeric",
            hour: "2-digit",
            minute: "2-digit",
          })}</time
        >
        {#if outcome !== "complete"}
          <span class="badge" data-tone="warn">{OUTCOME[outcome]}</span>
        {/if}
      </h2>
      <p>
        {fmtDuration(record.durationMs)} · {fmtBytes(
          record.totalBytes,
          store.unitBase,
        )} transferred
      </p>
    </div>
    <MoreMenu label="Result actions" danger>
      {#snippet children(select)}
        <button
          type="button"
          role="menuitem"
          tabindex="-1"
          onclick={() => select(onDelete)}
        >
          <span>{@html ICON.trash}</span>
          <span><strong>Delete this result</strong></span>
        </button>
      {/snippet}
    </MoreMenu>
    <button
      class="btn btn-icon btn-inset close-detail"
      type="button"
      aria-label="Close result"
      use:tooltip={"Close (Esc)"}
      onclick={onClose}
    >
      {@html ICON.close}
    </button>
  </header>

  <div class="detail-body">
    <ResultSummary
      {cards}
      {details}
      scope={shown}
      onscope={(id) => (shown = id)}
    />

    {#if profile.length}
      <section aria-labelledby={`result-${record.id}-latency`}>
        <h3 class="caps" id={`result-${record.id}-latency`}>
          Latency{latencyServer
            ? ` · ${serverLabel(latencyServer.server)}`
            : ""}
        </h3>
        <LatencyProfileView
          lanes={profile}
          variant="compact"
          label="Saved latency distributions"
        />
      </section>
    {/if}

    <details class="disclosure">
      <summary>Servers &amp; paths</summary>
      <div class="table-scroll">
        <table>
          <thead>
            <tr>
              <th scope="col">Server</th>
              <th scope="col">{STAGE.download.short}</th>
              <th scope="col">{STAGE.upload.short}</th>
              <th scope="col">Latency</th>
              <th scope="col">Throughput path</th>
              <th scope="col">Latency path</th>
            </tr>
          </thead>
          <tbody>
            {#each serverRows as row (row.id)}
              <tr>
                <th scope="row"
                  >{row.name}{#if row.host}<small>{row.host}</small>{/if}</th
                >
                <td>{row.down}</td>
                <td>{row.up}</td>
                <td>{row.latency}</td>
                <td>{row.throughputPath}</td>
                <td>{row.latencyPath}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    </details>

    {#if accounting.length}
      <details class="disclosure">
        <summary>Probe accounting</summary>
        <p class="hint">
          Timeouts: no reply before the deadline. Unfinished probes and failed
          sends are counted separately.
        </p>
        <ul class="accounting">
          {#each accounting as lane (lane.key)}
            {@const counts = probeAccountingSummary(lane)}
            <li
              data-tone={lane.tone}
              aria-label={`${lane.label}: ${probeAccountingDetails(lane)}`}
            >
              <strong>{lane.label}</strong>
              <span>{counts.replies}</span>
              <span
                >{counts.exceptions.join(" · ") ||
                  "No timeouts"}{#if lane.accountingComplete === false}<em
                    use:tooltip={PARTIAL_ACCOUNTING_HELP}
                    >· partial accounting</em
                  >{/if}</span
              >
            </li>
          {/each}
        </ul>
      </details>
    {/if}

    {#if issues.length}
      <details class="disclosure">
        <summary
          >Issues <span class="badge" data-tone="warn">{issues.length}</span
          ></summary
        >
        <ul class="issues">
          {#each issues as issue, index (index)}
            <li>{issue}</li>
          {/each}
        </ul>
      </details>
    {/if}

    <details class="disclosure">
      <summary>Build &amp; environment</summary>
      <dl class="kv">
        {#each environment as [label, value] (label)}
          <div>
            <dt>{label}</dt>
            <dd>{value}</dd>
          </div>
        {:else}
          <p class="hint">{MISSING}</p>
        {/each}
      </dl>
    </details>
  </div>
</article>

<style>
  .result-detail {
    display: flex;
    flex-direction: column;
    min-width: 0;
    background: var(--surface-1);
    container: detail / inline-size;
  }
  .result-detail:focus {
    outline: none;
  }
  .detail-head {
    position: sticky;
    top: 0;
    z-index: 2;
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-3) var(--space-4);
  }
  .title {
    flex: 1;
    min-width: 0;
  }
  h2 {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-1) var(--space-2);
    font: var(--w-strong) var(--type-md) / 1.3 var(--font-display);
    letter-spacing: var(--track-tight);
  }
  .title p {
    margin-top: 2px;
    color: var(--text-muted);
    font: var(--type-xs) var(--font-mono);
    font-variant-numeric: tabular-nums;
  }
  .back {
    display: none;
  }
  .detail-body {
    display: grid;
    gap: var(--space-4);
    padding: var(--space-4);
  }
  h3 {
    margin-bottom: var(--space-2);
  }
  .disclosure {
    border-top: 1px solid var(--border);
    padding-top: var(--space-3);
  }
  .disclosure > summary {
    cursor: pointer;
  }
  .disclosure[open] > summary {
    margin-bottom: var(--space-3);
  }
  .table-scroll {
    overflow-x: auto;
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font: var(--type-xs) / 1.4 var(--font-mono);
    font-variant-numeric: tabular-nums;
  }
  th,
  td {
    padding: 6px var(--space-2);
    border-bottom: 1px solid var(--border-subtle);
    text-align: start;
    vertical-align: top;
  }
  thead th {
    color: var(--text-soft);
    font-weight: var(--w-heavy);
    white-space: nowrap;
  }
  tbody th {
    font: var(--w-strong) var(--type-xs) var(--font-sans);
  }
  tbody small {
    display: block;
    color: var(--text-soft);
    font: var(--type-2xs) var(--font-mono);
  }
  .accounting,
  .issues {
    display: grid;
    gap: var(--space-1);
    margin-top: var(--space-2);
    font-size: var(--type-xs);
  }
  .accounting li {
    display: grid;
    grid-template-columns: minmax(7rem, auto) auto minmax(0, 1fr);
    gap: var(--space-2);
    padding-block: 4px;
    border-top: 1px solid var(--border-subtle);
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
  }
  .accounting strong {
    color: var(--tone);
  }
  .accounting em {
    margin-left: var(--space-1);
    color: var(--warn);
    font-style: normal;
  }
  .issues li {
    color: var(--text-muted);
    overflow-wrap: anywhere;
  }
  .kv {
    --kv-label: 7rem;
  }
  @container history (max-width: 820px) {
    .back {
      display: inline-flex;
    }
    .close-detail {
      display: none;
    }
  }
  @container detail (max-width: 460px) {
    .detail-head,
    .detail-body {
      padding-inline: var(--space-3);
    }
    .accounting li {
      grid-template-columns: minmax(0, 1fr) auto;
    }
    .accounting li > span:last-child {
      grid-column: 1 / -1;
    }
  }
</style>
