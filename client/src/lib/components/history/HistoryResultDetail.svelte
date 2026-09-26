<script lang="ts">
  import Icon from "../Icon.svelte";
  import { httpProtocolLabel } from "../../runner/paths";
  import { serverLabel, serverName } from "../../presentation/serverAppearance";
  import { tooltip } from "../../actions/tooltip";
  import { fmtBytes, fmtDuration, resultRate } from "../../format";
  import { formatHistoryRate, formatLatency } from "../../history/format";
  import type { HistoryRecord } from "../../history/types";
  import { latencyLanes, transferredBytes } from "../../runner/measure";
  import { store } from "../../state/store.svelte";
  import {
    OUTCOME,
    STAGE,
    MISSING,
    reasonLabel,
    TRANSPORT,
    transportLabel,
  } from "../../presentation/vocabulary";
  import type { TransportKind } from "../../runner/contract";
  import {
    summaryCards,
    summaryEvidence,
  } from "../../presentation/resultSummary";
  import {
    LATENCY_LANES,
    savedLatencyHasProbeEvidence,
    PARTIAL_ACCOUNTING_HELP,
    probeAccountingDetails,
    probeAccountingSummary,
    hasProbeAccountingNotice,
  } from "../latencyProfile";
  import Disclosure from "../Disclosure.svelte";
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
  const result = $derived(record.result);
  const run = $derived(result.multiServer);
  const details = $derived(run.selection.length > 1 ? run : null);
  const units = $derived({ base: store.unitBase, kind: store.unitKind });
  const completed = $derived(new Date(record.completedAt));

  const cards = $derived.by(() => {
    const evidence = summaryEvidence(
      result.stages,
      {
        download: result.download,
        upload: result.upload,
        bidirectional: result.bidirectional,
        latency: result.latency,
        added: result.addedLatency,
      },
      details,
      shown,
      details?.latencyFocus,
    );
    return summaryCards(
      evidence,
      (value) => resultRate(value, units),
      store.unitBase,
      store.showWireEstimates,
    );
  });

  // Latency follows the chosen server when it measured latency, else the headline server.
  const latencyServer = $derived(
    run.servers.find(
      (server) => server.server.id === shown && server.latencyTarget,
    ) ?? run.servers.find((server) => server.server.id === run.latencyFocus),
  );
  const lanes = $derived.by(() => {
    const saved = latencyLanes(
      latencyServer?.latencyByStage ?? result.latencyByStage,
    );
    return LATENCY_LANES.flatMap((meta) => {
      const lane = saved[meta.key];
      return lane ? [{ ...meta, ...lane }] : [];
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
      latencyServer?.latencyTarget?.transport ?? null,
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

  const accountingFacts = $derived(
    accounting
      .flatMap((lane) =>
        probeAccountingSummary(lane).exceptions.map(
          (exception) => `${lane.label} ${exception}`,
        ),
      )
      .join(" · ") || "No timeouts or failed sends",
  );

  function path(
    role: "throughput" | "latency",
    kind: string | null | undefined,
    protocol?: string | null,
    browserProtocol?: string,
  ): string {
    const mechanism =
      kind && kind in TRANSPORT
        ? transportLabel(kind as TransportKind, role)
        : "Not recorded";
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
    run.selection.map((server) => {
      const measured = run.servers.find(
        (entry) => entry.server.id === server.id,
      );
      return {
        id: server.id,
        name: serverLabel(server),
        host: URL.parse(server.url)?.host ?? "",
        down: rate(measured?.download?.reportedBytesPerSec),
        up: rate(measured?.upload?.reportedBytesPerSec),
        latency: formatLatency(measured?.latency?.reportedMs),
        throughputPath: measured
          ? path(
              "throughput",
              measured.throughput.transport,
              measured.throughput.protocol,
              measured.throughput.browserProtocol,
            )
          : "Not measured",
        latencyPath: measured?.latencyTarget
          ? path("latency", measured.latencyTarget.transport)
          : "Not measured",
      };
    }),
  );
  const issues = $derived(
    run.failures.map((failure) =>
      [
        serverName(run.selection, failure.serverId),
        STAGE[failure.stage].label +
          (failure.scope === "latency" ? " latency" : ""),
        reasonLabel(failure.reason),
      ].join(" · "),
    ),
  );
  const ipVersion = $derived(
    run.servers.find((server) => server.server.id === run.latencyFocus)
      ?.throughput.clientIpVersion,
  );
  const environment = $derived(
    [
      ["IP family", ipVersion ? `IPv${ipVersion}` : null],
      ["Client build", record.build],
      ["Server engine", record.engine],
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
        {#if result.outcome !== "complete"}
          <span class="badge" data-tone="warn">{OUTCOME[result.outcome]}</span>
        {/if}
      </h2>
      <p>
        {fmtDuration(result.durationMs)} · {fmtBytes(
          transferredBytes(run),
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
          <span><Icon name="trash" /></span>
          <span><strong>Delete this result</strong></span>
        </button>
      {/snippet}
    </MoreMenu>
    <button
      class="btn btn-icon btn-inset close-detail"
      type="button"
      aria-label="Close result"
      {@attach tooltip(() => "Close (Esc)")}
      onclick={onClose}
    >
      <Icon name="close" />
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

    <div class="sections">
      <Disclosure
        class="surface-inset"
        title="Servers & paths"
        facts={serverRows.length > 1
          ? serverRows.map((row) => row.name).join(", ")
          : `${serverRows[0]?.name ?? MISSING} · ${serverRows[0]?.throughputPath ?? MISSING}`}
      >
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
      </Disclosure>

      {#if accounting.length}
        <Disclosure
          class="surface-inset"
          title="Probe accounting"
          facts={accountingFacts}
        >
          <p class="hint">
            Timeouts: no reply before the deadline. Unfinished probes and failed
            sends are counted separately.{accounting.some(
              (lane) => lane.accountingComplete === false,
            )
              ? ` Partial accounting: ${PARTIAL_ACCOUNTING_HELP}`
              : ""}
          </p>
          <ul class="accounting">
            {#each accounting as lane (lane.key)}
              {@const counts = probeAccountingSummary(lane)}
              <li
                data-tone={lane.key}
                aria-label={`${lane.label}: ${probeAccountingDetails(lane)}`}
              >
                <strong>{lane.label}</strong>
                <span>{counts.replies}</span>
                <span
                  >{[
                    ...counts.exceptions,
                    ...(lane.accountingComplete === false
                      ? ["partial accounting"]
                      : []),
                  ].join(" · ") || "No timeouts"}</span
                >
              </li>
            {/each}
          </ul>
        </Disclosure>
      {/if}

      {#if issues.length}
        <Disclosure
          class="surface-inset"
          title="Issues"
          facts={issues.length > 1
            ? `${issues[0]} and ${issues.length - 1} more`
            : issues[0]}
        >
          {#snippet aside()}
            <span class="badge" data-tone="warn">{issues.length}</span>
          {/snippet}
          <ul class="issues">
            {#each issues as issue, index (index)}
              <li>{issue}</li>
            {/each}
          </ul>
        </Disclosure>
      {/if}

      <Disclosure
        class="surface-inset"
        title="Build & environment"
        facts={environment.map(([, value]) => value).join(" · ") || MISSING}
      >
        <dl class="kv">
          {#each environment as [label, value] (label)}
            <div>
              <dt>{label}</dt>
              <dd>{value}</dd>
            </div>
          {/each}
        </dl>
      </Disclosure>
    </div>
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
  .sections {
    display: grid;
    gap: var(--space-2);
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
