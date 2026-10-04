<script lang="ts">
  import Icon from "../Icon.svelte";
  import { httpProtocolLabel } from "../../runner/paths";
  import { keyHint, term, tipGroup, tooltip } from "../../actions/tooltip";
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
    reasonLabel,
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
  import { throughputScales } from "../../presentation/scales";
  import {
    LATENCY_LANES,
    savedLatencyHasProbeEvidence,
    PARTIAL_ACCOUNTING_HELP,
    probeAccountingDetails,
    probeAccountingSummary,
    hasProbeAccountingNotice,
  } from "../latencyProfile";
  import ResultSummary from "../ResultSummary.svelte";
  import ServerScope from "../ServerScope.svelte";
  import { allServersLabel } from "../../presentation/serverAppearance";
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
  // A saved result reads in one tier, as the page did: the tier its own peaks set.
  const units = $derived({
    base: store.unitBase,
    kind: store.unitKind,
    tier: throughputScales(
      [],
      result,
      store.config.visualization.throughputMaxBytesPerSec,
      store.unitBase,
      store.unitKind,
    ).unitIndex,
  });
  const completed = $derived(new Date(record.completedAt));

  const cards = $derived.by(() => {
    const evidence = summaryEvidence(
      result.stages,
      {
        download: result.download,
        upload: result.upload,
        bidirectional: result.bidirectional,
        latency: result.latency,
        idle: (latencyServer?.latencyByStage ?? result.latencyByStage).latency,
        added: result.addedLatency,
      },
      details,
      shown,
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
  ) {
    const mechanism =
      kind && kind in TRANSPORT
        ? transportLabel(kind as TransportKind, role)
        : "Not recorded";
    const observed = browserProtocol && httpProtocolLabel(browserProtocol);
    const endpoint = protocol && httpProtocolLabel(protocol);
    return {
      value: mechanism,
      aside:
        observed &&
        endpoint &&
        observed !== endpoint &&
        protocol !== "negotiated"
          ? `${observed}, endpoint ${endpoint}`
          : observed || endpoint || "",
    };
  }
  const unmeasured = { value: "Not measured", aside: "" };
  const rate = (value: number | null | undefined) => formatRate(value, units);
  const serverRows = $derived(
    run.selection.map((server) => {
      const measured = run.servers.find(
        (entry) => entry.server.id === server.id,
      );
      const failure = run.failures.find(
        (entry) => entry.serverId === server.id,
      );
      return {
        id: server.id,
        name: server.name,
        location: server.location,
        url: server.url,
        failure: failure && reasonLabel(failure.reason),
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
          : unmeasured,
        latencyPath: measured?.latencyTarget
          ? path("latency", measured.latencyTarget.transport)
          : unmeasured,
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

{#snippet pathRow(
  label: string,
  tip: string,
  path: { value: string; aside: string },
)}
  <div>
    <dt {@attach tooltip(() => tip)}>{label}</dt>
    <dd>
      {path.value}{#if path.aside}<span class="aside">{path.aside}</span>{/if}
    </dd>
  </div>
{/snippet}

<article
  bind:this={region}
  class="result-detail"
  aria-labelledby={`result-${record.id}-title`}
  tabindex="-1"
>
  <header class="sheet-head detail-head page-fill">
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
      </h2>
      {#if result.outcome !== "complete"}
        <span class="outcome"
          ><span
            class="status-dot inline"
            data-tone={STATUS_TONE[result.outcome]}
          ></span>{OUTCOME[result.outcome]}</span
        >
      {/if}
    </div>
    <dl class="head-facts">
      <div>
        <dt>Duration</dt>
        <dd>{fmtDuration(result.durationMs)}</dd>
      </div>
      <div>
        <dt>Transferred</dt>
        <dd>{fmtBytes(transferredBytes(run), store.unitBase)}</dd>
      </div>
    </dl>
    <div class="head-actions">
      {#if details}
        <ServerScope
          quiet
          servers={details.selection}
          value={shown}
          onchange={(id) => (shown = id)}
          disabledIds={details.selection
            .filter(
              ({ id }) =>
                !details.servers.some(({ server }) => server.id === id),
            )
            .map(({ id }) => id)}
          aggregate={allServersLabel(
            details.selection.length,
            details.participants.length,
            details.servers.length,
          )}
          label="Servers shown in this result"
        />
      {/if}
      <button
        class="btn btn-icon btn-quiet"
        type="button"
        aria-label="Delete this result"
        {@attach tooltip(() => "Delete this result")}
        onclick={(event) => onDelete(event.currentTarget)}
      >
        <Icon name="trash" />
      </button>
      <button
        class="btn btn-icon btn-quiet close-detail"
        type="button"
        aria-label="Close result"
        {@attach tooltip(() => `Close${keyHint("Esc")}`)}
        onclick={onClose}
      >
        <Icon name="close" />
      </button>
    </div>
  </header>

  <div class="detail-body">
    <ResultSummary
      {cards}
      {details}
      scope={shown}
      issues={serverIssues(run, shown)}
    />

    <div class="facts">
      {#if profile.length}
        <div class="latency">
          <LatencyProfileView
            lanes={profile}
            variant="compact"
            label="Saved latency distributions"
            added={latencyServer?.addedLatency ?? result.addedLatency}
            stability={(latencyServer?.latency ?? result.latency)
              ?.stabilityPct ?? null}
            source={multiple ? latencyServer?.server.name : undefined}
          />
        </div>
      {/if}

      {#each serverRows as row (row.id)}
        <section class="group">
          <div class="group-head">
            <h3>{multiple ? row.name : "Server"}</h3>
          </div>
          <dl class="kv" data-tip-group {@attach tipGroup}>
            {#if !multiple}<div>
                <dt>Name</dt>
                <dd>{row.name}</dd>
              </div>{/if}
            {#if row.location}<div>
                <dt>Location</dt>
                <dd>{row.location}</dd>
              </div>{/if}
            {#if row.url}<div>
                <dt>Address</dt>
                <dd>{row.url}</dd>
              </div>{/if}
            {#if row.failure}<div>
                <dt>Status</dt>
                <dd class="status">
                  <span class="status-dot inline" data-tone="err"
                  ></span>{row.failure}
                </dd>
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
            {@render pathRow(
              "Throughput path",
              JARGON.throughputPath,
              row.throughputPath,
            )}
            {@render pathRow(
              "Latency path",
              JARGON.latencyPath,
              row.latencyPath,
            )}
          </dl>
        </section>
      {/each}

      {#if accounting.length}
        <section class="group">
          <h3>
            <span
              {@attach term(() =>
                accounting.some((lane) => lane.accountingComplete === false)
                  ? `${JARGON.probeAccounting}\nPartial: ${PARTIAL_ACCOUNTING_HELP}`
                  : JARGON.probeAccounting,
              )}>Probes</span
            >
          </h3>
          <dl class="kv" data-tip-group {@attach tipGroup}>
            {#each accounting as lane (lane.key)}
              {@const counts = probeAccountingSummary(lane)}
              <div
                data-tone={lane.key}
                aria-label={`${lane.label}: ${probeAccountingDetails(lane)}`}
              >
                <dt>{lane.label}</dt>
                <dd>
                  {[counts.replies, ...counts.exceptions].join(
                    ", ",
                  )}{#if lane.accountingComplete === false}<span class="aside"
                      >Partial</span
                    >{/if}
                </dd>
              </div>
            {/each}
          </dl>
        </section>
      {/if}

      <section class="group">
        <h3>Build</h3>
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
    container: detail / inline-size;
  }
  /* Inside its pane's scroller, the head shares the pane's gutter. */
  .detail-head {
    position: sticky;
    top: 0;
    z-index: 2;
    overflow: visible;
    scrollbar-gutter: auto;
    padding-inline-end: calc(var(--panel-pad) - 12px);
  }
  .title {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-1) var(--space-3);
    min-width: 0;
  }
  /* The result's time is what the pane is about, so it takes a sheet title's size; the facts sit beside it. */
  h2 {
    min-width: 0;
    font: var(--role-panel-title);
    letter-spacing: var(--track-tight);
  }
  /* Pulled out by its padding, so the arrow starts on the title's edge. */
  .back {
    display: none;
    margin-left: calc(-1 * (var(--space-3) + var(--hit-pad)));
  }
  .detail-body {
    display: grid;
    gap: var(--space-5);
    padding: var(--space-1) var(--panel-pad) var(--space-6);
  }
  .detail-body > :global(.result-summary) {
    max-width: none;
  }
  /* A saved result's three cards run three across or one to a row, never two and an orphan (3 × 240 px + 2 gaps). */
  @container results (width < 768px) {
    .detail-body :global(.result-cards:has(> :nth-child(3))) {
      grid-template-columns: minmax(0, 1fr);
    }
  }
  /* Fact groups share the width in columns, so a long server list stays beside the rest. */
  .facts {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(100%, 300px), 1fr));
    gap: var(--space-5) var(--space-4);
    align-items: start;
  }
  .latency {
    grid-column: 1 / -1;
  }
  /* On the reason's first baseline, so its label reads on the same line. */
  .status {
    display: flex;
    align-items: baseline;
    gap: var(--space-2);
  }
  @container history (max-width: 820px) {
    .back {
      display: inline-flex;
    }
    .close-detail {
      display: none;
    }
  }
  /* A phone keeps the result's reading height: the head scrolls away under History's own. */
  @container detail (max-width: 560px) {
    .detail-head {
      position: static;
      animation: none;
    }
    .title,
    .head-facts {
      order: 3;
      flex-basis: 100%;
    }
    .head-facts {
      order: 4;
    }
  }
</style>
