<script lang="ts">
  import Icon from "../Icon.svelte";
  import { httpProtocolLabel } from "../../runner/paths";
  import { serverLabel } from "../../presentation/serverAppearance";
  import { tipGroup, tooltip } from "../../actions/tooltip";
  import {
    fmtBytes,
    fmtDuration,
    formatLatency,
    formatRate,
  } from "../../format";
  import type { HistoryRecord } from "../../history/types";
  import { latencyLanes, transferredBytes } from "../../runner/measure";
  import { store } from "../../state/store.svelte";
  import {
    JARGON,
    OUTCOME,
    STAGE,
    STATUS_TONE,
    TRANSPORT,
    transportLabel,
  } from "../../presentation/vocabulary";
  import type { TransportKind } from "../../runner/contract";
  import {
    serverIssues,
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
  const multiple = $derived(run.selection.length > 1);
  const details = $derived(multiple ? run : null);
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
    return summaryCards(evidence, units, store.showWireEstimates);
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
  const rate = (value: number | null | undefined) => formatRate(value, units);
  const serverRows = $derived(
    run.selection.map((server) => {
      const measured = run.servers.find(
        (entry) => entry.server.id === server.id,
      );
      return {
        id: server.id,
        name: serverLabel(server),
        url: server.url,
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
  const ipVersion = $derived(
    run.servers.find((server) => server.server.id === run.latencyFocus)
      ?.throughput.clientIpVersion,
  );
  const environment = $derived(
    [
      ["Client", record.build],
      ["Server", record.engine],
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
          <span class="badge" data-tone={STATUS_TONE[result.outcome]}
            >{OUTCOME[result.outcome]}</span
          >
        {/if}
      </h2>
      <p>
        {fmtDuration(result.durationMs)} · {fmtBytes(
          transferredBytes(run),
          store.unitBase,
        )} transferred
      </p>
    </div>
    <button
      class="btn btn-icon btn-inset"
      type="button"
      aria-label="Delete this result"
      {@attach tooltip(() => "Delete this result")}
      onclick={(event) => onDelete(event.currentTarget)}
    >
      <Icon name="trash" />
    </button>
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
      issues={serverIssues(run, shown)}
    />

    {#if profile.length}
      <section class="group" aria-labelledby={`result-${record.id}-latency`}>
        <h3 class="caps" id={`result-${record.id}-latency`}>
          Latency{#if multiple && latencyServer}<span class="name">
              · {serverLabel(latencyServer.server)}</span
            >{/if}
        </h3>
        <LatencyProfileView
          lanes={profile}
          variant="compact"
          label="Saved latency distributions"
        />
      </section>
    {/if}

    <div class="facts">
      {#if accounting.length}
        <section class="group">
          <h3 class="caps">Probes</h3>
          <dl class="kv" data-tip-group {@attach tipGroup}>
            {#each accounting as lane (lane.key)}
              {@const counts = probeAccountingSummary(lane)}
              <div
                data-tone={lane.key}
                aria-label={`${lane.label}: ${probeAccountingDetails(lane)}`}
                {@attach tooltip(() =>
                  lane.accountingComplete === false
                    ? `${JARGON.probeAccounting}\nPartial: ${PARTIAL_ACCOUNTING_HELP}`
                    : JARGON.probeAccounting,
                )}
              >
                <dt>{lane.label}</dt>
                <dd>
                  {[
                    counts.replies,
                    ...counts.exceptions,
                    ...(lane.accountingComplete === false
                      ? ["partial accounting"]
                      : []),
                  ].join(" · ")}
                </dd>
              </div>
            {/each}
          </dl>
        </section>
      {/if}

      {#each serverRows as row (row.id)}
        <section class="group">
          <h3 class="caps">
            Server{#if multiple}<span class="name"> · {row.name}</span>{/if}
          </h3>
          <dl class="kv" data-tip-group {@attach tipGroup}>
            {#if !multiple}<div>
                <dt>Name</dt>
                <dd>{row.name}</dd>
              </div>{/if}
            {#if row.url}<div>
                <dt>Address</dt>
                <dd>{row.url}</dd>
              </div>{/if}
            {#if ipVersion && row.id === run.latencyFocus}<div>
                <dt>IP family</dt>
                <dd>IPv{ipVersion}</dd>
              </div>{/if}
            {#if multiple}
              <div>
                <dt>{STAGE.download.short}</dt>
                <dd>{row.down}</dd>
              </div>
              <div>
                <dt>{STAGE.upload.short}</dt>
                <dd>{row.up}</dd>
              </div>
              <div>
                <dt>Latency</dt>
                <dd>{row.latency}</dd>
              </div>
            {/if}
            <div {@attach tooltip(() => JARGON.throughputPath)}>
              <dt>Throughput path</dt>
              <dd>{row.throughputPath}</dd>
            </div>
            <div {@attach tooltip(() => JARGON.latencyPath)}>
              <dt>Latency path</dt>
              <dd>{row.latencyPath}</dd>
            </div>
          </dl>
        </section>
      {/each}

      <section class="group">
        <h3 class="caps">Build</h3>
        <dl class="kv" data-tip-group {@attach tipGroup}>
          {#each environment as [label, value] (label)}
            <div>
              <dt>{label}</dt>
              <dd>{value}</dd>
            </div>
          {/each}
        </dl>
      </section>
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
  .detail-body > :global(.result-summary) {
    max-width: none;
  }
  h3 > .name {
    text-transform: none;
    letter-spacing: 0;
  }
  .facts {
    display: grid;
    gap: var(--space-4);
    align-content: start;
  }
  @container detail (min-width: 1000px) {
    .detail-body {
      grid-template-columns: minmax(0, 3fr) minmax(0, 2fr);
      align-items: start;
    }
    .detail-body > :global(:first-child) {
      grid-column: 1 / -1;
    }
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
  }
</style>
