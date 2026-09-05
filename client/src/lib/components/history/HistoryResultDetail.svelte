<script lang="ts">
  import { tooltip } from "../../actions/tooltip";
  import { ICON } from "../../constants";
  import {
    formatDuration,
    formatHistoryBytes,
    formatHistoryRate,
    formatLatency,
    formatPercent,
  } from "../../history/format";
  import type { HistoryRecord, ThroughputSnapshot } from "../../history/types";
  import { store } from "../../state/store.svelte";
  import {
    LATENCY_LANES,
    savedLatencyHasProbeEvidence,
    probeAccountingHelp,
    probeAccountingDetails,
    hasProbeAccountingNotice,
    type LatencyProfileViewLane,
    type LatencyProfileTone,
  } from "../latencyProfile";
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
  const units = $derived({ base: store.unitBase, kind: store.unitKind });
  const completedDate = $derived(new Date(record.completedAt));
  const partial = $derived(
    Object.values(record.stages.latency.lanes).some(
      (lane) =>
        lane != null &&
        record.schemaVersion === 2 &&
        lane.accountingComplete !== true,
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

  function bytes(result: ThroughputSnapshot): string {
    return formatHistoryBytes(result.totalBytes, store.unitBase);
  }

  function throughputCard(
    key: "download" | "upload",
    result: ThroughputSnapshot | null,
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
    const stage = record.stages.bidirectional;
    if (!stage.down && !stage.up) return null;
    const complete = stage.down && stage.up;
    const measuredLane = stage.down ?? stage.up!;
    const directions = [
      stage.down ? `Down ${rate(stage.down.reportedBytesPerSec)}` : null,
      stage.up ? `Up ${rate(stage.up.reportedBytesPerSec)}` : null,
    ].filter((value): value is string => value !== null);
    return {
      key: "bidirectional",
      label: "Bidirectional",
      icon: ICON.bidirectional,
      tone: "bidirectional",
      value: complete
        ? rate(stage.down!.reportedBytesPerSec + stage.up!.reportedBytesPerSec)
        : rate(measuredLane.reportedBytesPerSec),
      detail: directions.join(" · "),
    };
  }

  const throughputCards = $derived<ThroughputCard[]>(
    [
      throughputCard("download", record.stages.download.result),
      throughputCard("upload", record.stages.upload.result),
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
      const snapshot = record.stages.latency.lanes[meta.key];
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
              accountingComplete:
                record.schemaVersion === 2
                  ? (snapshot.accountingComplete ?? false)
                  : undefined,
              accountingLegacy:
                record.schemaVersion === 2 &&
                snapshot.accountingComplete === undefined,
              timeoutRatio: snapshot.timeoutRatio ?? snapshot.lossRatio ?? null,
            },
          ]
        : [];
    }),
  );
  const latencyProfiles = $derived(savedLanes.filter(usefulLane));
  const probeTimeoutLanes = $derived(
    savedLatencyHasProbeEvidence(record.transport.latency.kind)
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
  {/if}

  {#if latencyProfiles.length}
    <section
      class="detail-section"
      aria-labelledby={`result-${record.id}-latency`}
    >
      <header class="section-head">
        <span aria-hidden="true">{@html ICON.ping}</span>
        <h3 id={`result-${record.id}-latency`}>Responsiveness</h3>
      </header>
      <div class="section-body responsiveness-body">
        {#if record.stages.latency.result}
          <dl class="idle-summary" aria-label="Idle latency result">
            <div>
              <dt>Idle result</dt>
              <dd>{formatLatency(record.stages.latency.result.reportedMs)}</dd>
            </div>
            <div>
              <dt>Median (p50)</dt>
              <dd>{formatLatency(record.stages.latency.result.p50Ms)}</dd>
            </div>
            <div>
              <dt>p95</dt>
              <dd>{formatLatency(record.stages.latency.result.p95Ms)}</dd>
            </div>
            <div>
              <dt>Stability</dt>
              <dd>
                {formatPercent(
                  record.stages.latency.result.stabilityScore * 100,
                  0,
                )}
              </dd>
            </div>
          </dl>
        {/if}
        <LatencyProfileView
          lanes={latencyProfiles}
          variant="compact"
          label="Saved latency distributions"
          jitterDescription={record.schemaVersion === 1
            ? "Legacy variation estimate calculated from chart-bucket medians; it is not comparable to the current raw-reply calculation."
            : undefined}
        />
      </div>
    </section>
  {/if}

  {#if record.schemaVersion === 1}
    <p class="detail-section">
      Legacy measurement: latency profiles and timeout percentages use the
      earlier calculation.
    </p>
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
          use:tooltip={"Application probes whose reply deadline expired. WebTransport uses datagrams; WebSocket uses a reliable stream. Neither identifies physical or directional IP packet loss. Interrupted and locally rejected sends are excluded."}
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
              <small>{lane.details}</small>
              {#if lane.accountingComplete === false}
                <small role="note" use:tooltip={probeAccountingHelp(lane)}
                  >Partial accounting</small
                >
              {/if}
            </span>
            <em>{lane.value}</em>
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
    grid-template-columns: repeat(auto-fit, minmax(135px, 1fr));
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
    padding: 8px;
    border-top: 1px solid color-mix(in srgb, var(--tone) 40%, var(--border));
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
    font: 500 9px var(--font-mono);
  }
  .probe-timeouts-lanes em {
    color: var(--tone);
    font: 700 var(--type-sm) var(--font-mono);
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
