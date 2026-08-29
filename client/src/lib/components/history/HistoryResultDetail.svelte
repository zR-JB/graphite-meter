<script lang="ts">
  import { ICON } from "../../constants";
  import {
    formatDuration,
    formatHistoryBytes,
    formatHistoryRate,
    stageStatusLabel,
  } from "../../history/format";
  import type {
    HistoryRecordV1,
    ThroughputSnapshot,
  } from "../../history/types";
  import { store } from "../../state/store.svelte";
  import { savedLatencyLossVisible } from "../latencyProfile";
  import LatencyProfileSummary, {
    type FinalizedLatencyLane,
  } from "./LatencyProfileSummary.svelte";

  interface Props {
    record: HistoryRecordV1;
    onClose: () => void;
    onDelete: () => void;
    heading?: HTMLElement;
  }

  let { record, onClose, onDelete, heading = $bindable() }: Props = $props();
  const units = $derived({ base: store.unitBase, kind: store.unitKind });
  const completedDate = $derived(new Date(record.completedAt));
  const partial = $derived(
    record.failures.length > 0 ||
      [
        record.stages.latency.status,
        record.stages.download.status,
        record.stages.upload.status,
        record.stages.bidirectional.status,
      ].some((status) => status === "partial" || status === "failed"),
  );

  const loadedProfiles = $derived<FinalizedLatencyLane[]>(
    [
      ["download", "Loaded down", ICON.download],
      ["upload", "Loaded up", ICON.upload],
      ["bidirectional", "Loaded bi-dir", ICON.bidirectional],
    ].flatMap(([key, label, icon]) => {
      const lane =
        record.stages.latency.lanes[
          key as "download" | "upload" | "bidirectional"
        ];
      return lane
        ? [{ key, label, icon, ...lane } as FinalizedLatencyLane]
        : [];
    }),
  );
  const profileLanes = $derived<FinalizedLatencyLane[]>(
    [
      record.stages.latency.lanes.latency
        ? {
            key: "latency" as const,
            label: "Idle",
            icon: ICON.ping,
            ...record.stages.latency.lanes.latency,
            headline: record.stages.latency.result
              ? {
                  p50Ms: record.stages.latency.result.p50Ms,
                  p95Ms: record.stages.latency.result.p95Ms,
                  stabilityScore: record.stages.latency.result.stabilityScore,
                }
              : undefined,
          }
        : null,
      ...loadedProfiles,
    ].filter((lane): lane is FinalizedLatencyLane => lane !== null),
  );
  const hasResponsivenessData = $derived(
    profileLanes.some((lane) =>
      [lane.min, lane.max, lane.p10, lane.p90, lane.center, lane.jitter].some(
        (value) => value != null,
      ),
    ),
  );
  const throughputRows = $derived(
    [
      record.stages.download.status !== "not-run"
        ? {
            key: "download",
            label: "Download",
            icon: ICON.download,
            status: record.stages.download.status,
            value: record.stages.download.result?.reportedBytesPerSec ?? null,
            detail: record.stages.download.result
              ? `${bytes(record.stages.download.result)} transferred`
              : "",
          }
        : null,
      record.stages.upload.status !== "not-run"
        ? {
            key: "upload",
            label: "Upload",
            icon: ICON.upload,
            status: record.stages.upload.status,
            value: record.stages.upload.result?.reportedBytesPerSec ?? null,
            detail: record.stages.upload.result
              ? `${bytes(record.stages.upload.result)} transferred`
              : "",
          }
        : null,
      record.stages.bidirectional.status !== "not-run"
        ? {
            key: "bidirectional",
            label: "Bidirectional",
            icon: ICON.bidirectional,
            status: record.stages.bidirectional.status,
            value:
              record.stages.bidirectional.down && record.stages.bidirectional.up
                ? record.stages.bidirectional.down.reportedBytesPerSec +
                  record.stages.bidirectional.up.reportedBytesPerSec
                : null,
            detail:
              record.stages.bidirectional.down || record.stages.bidirectional.up
                ? `Down ${record.stages.bidirectional.down ? rate(record.stages.bidirectional.down.reportedBytesPerSec) : "Unavailable"} · Up ${record.stages.bidirectional.up ? rate(record.stages.bidirectional.up.reportedBytesPerSec) : "Unavailable"}`
                : "",
          }
        : null,
    ].filter((row) => row !== null),
  );
  const showLatencyLoss = $derived(
    savedLatencyLossVisible(record.transport.latency.kind),
  );

  function rate(value: number | null | undefined): string {
    return formatHistoryRate(value, units);
  }
  function bytes(result: ThroughputSnapshot | null): string {
    return result
      ? formatHistoryBytes(result.totalBytes, store.unitBase)
      : "Unavailable";
  }
  function transport(value: string | null): string {
    return value?.replaceAll("-", " ") ?? "Unavailable";
  }
</script>

<article class="result-detail" aria-labelledby={`result-${record.id}-title`}>
  <header class="detail-head">
    <button
      class="close-detail"
      type="button"
      aria-label="Close result"
      onclick={onClose}
    >
      <span>{@html ICON.close}</span>Close result
    </button>
    <div class="detail-title">
      <span class="eyebrow">Saved result</span>
      <h2 id={`result-${record.id}-title`} tabindex="-1" bind:this={heading}>
        <time datetime={completedDate.toISOString()}>
          {completedDate.toLocaleDateString(undefined, {
            weekday: "short",
            month: "short",
            day: "numeric",
            year: "numeric",
          })}
          <span
            >{completedDate.toLocaleTimeString(undefined, {
              hour: "2-digit",
              minute: "2-digit",
            })}</span
          >
        </time>
      </h2>
    </div>
  </header>

  <div class="run-facts" aria-label="Run summary">
    <div>
      <span>Completion</span><strong class:partial
        >{partial ? "Partial" : "Complete"}</strong
      >
    </div>
    <div>
      <span>Actual duration</span><strong
        >{formatDuration(record.durationMs)}</strong
      >
    </div>
    <div>
      <span>Transferred</span><strong
        >{formatHistoryBytes(record.totalBytes, store.unitBase)}</strong
      >
    </div>
  </div>

  <details class="detail-group" open>
    <summary class="section-head">
      <span class="section-mark" data-tone="bidirectional"
        >{@html ICON.bidirectional}</span
      >
      <h3 id={`result-${record.id}-throughput`}>Throughput lanes</h3>
    </summary>
    <div class="detail-content phase-stack">
      {#each throughputRows as row (row.key)}
        <div class="phase-row" data-tone={row.key}>
          <div class="phase-name">
            <span>{@html row.icon}</span><strong>{row.label}</strong>
          </div>
          <div class="phase-reading">
            <strong
              >{row.value == null
                ? stageStatusLabel(row.status)
                : rate(row.value)}</strong
            >
            {#if row.detail}<small>{row.detail}</small>{/if}
          </div>
        </div>
      {/each}
      {#if throughputRows.length === 0}<p class="not-run-note">
          No throughput stages were run.
        </p>{/if}
    </div>
  </details>

  <details class="detail-group">
    <summary class="section-head">
      <span class="section-mark" data-tone="latency">{@html ICON.ping}</span>
      <h3 id={`result-${record.id}-latency`}>Responsiveness</h3>
    </summary>
    <div class="detail-content">
      {#if !hasResponsivenessData}
        <div class="stage-unavailable">
          <strong>{stageStatusLabel(record.stages.latency.status)}</strong>
          <span>
            {record.stages.latency.status === "not-run"
              ? "Responsiveness was not included in this run."
              : "No usable responsiveness measurement was retained."}
          </span>
        </div>
      {:else}
        <LatencyProfileSummary
          lanes={profileLanes}
          showLoss={showLatencyLoss}
        />
      {/if}
    </div>
  </details>

  <details class="detail-group">
    <summary class="section-head">
      <span class="section-mark">{@html ICON.info}</span>
      <h3 id={`result-${record.id}-context`}>Run context</h3>
      <span class="section-preview">Server, transport & build</span>
    </summary>
    <dl class="detail-content context-list">
      <div>
        <dt>Server</dt>
        <dd>
          {record.server.name}{record.server.location
            ? ` · ${record.server.location}`
            : ""}
        </dd>
      </div>
      <div>
        <dt>Throughput transport</dt>
        <dd>{transport(record.transport.throughput.kind)}</dd>
      </div>
      <div>
        <dt>Throughput protocol</dt>
        <dd>{record.transport.throughput.protocol ?? "Unavailable"}</dd>
      </div>
      <div>
        <dt>Latency transport</dt>
        <dd>{transport(record.transport.latency.kind)}</dd>
      </div>
      <div>
        <dt>Latency protocol</dt>
        <dd>{record.transport.latency.protocol ?? "Unavailable"}</dd>
      </div>
      <div>
        <dt>IP family</dt>
        <dd>{record.ipVersion ? `IPv${record.ipVersion}` : "Unavailable"}</dd>
      </div>
      <div>
        <dt>Client build</dt>
        <dd>{record.client.build}</dd>
      </div>
      <div>
        <dt>Server engine</dt>
        <dd>{record.server.engine}</dd>
      </div>
    </dl>
  </details>

  {#if record.failures.length}
    <details class="detail-group issues" open>
      <summary class="section-head">
        <span class="section-mark issue-mark">!</span>
        <h3 id={`result-${record.id}-issues`}>Structured failures</h3>
        <span class="section-preview">{record.failures.length}</span>
      </summary>
      <ul class="detail-content">
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
    </details>
  {/if}

  {#if record.wireEstimates && store.showWireEstimates}
    <details class="detail-group">
      <summary class="section-head">
        <span class="section-mark wire-mark">W</span>
        <h3 id={`result-${record.id}-wire`}>Wire-rate snapshot</h3>
        <span class="section-preview">Optional estimate</span>
      </summary>
      <div class="detail-content">
        <p class="wire-note">
          Stored display estimate · model {record.wireEstimates.version}
        </p>
        <dl class="wire-list">
          <div>
            <dt>Download</dt>
            <dd>{rate(record.wireEstimates.downloadBytesPerSec)}</dd>
          </div>
          <div>
            <dt>Upload</dt>
            <dd>{rate(record.wireEstimates.uploadBytesPerSec)}</dd>
          </div>
          <div>
            <dt>Bidirectional</dt>
            <dd>{rate(record.wireEstimates.bidirectionalBytesPerSec)}</dd>
          </div>
        </dl>
      </div>
    </details>
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
  .detail-head {
    position: sticky;
    top: 0;
    z-index: 3;
    display: grid;
    gap: var(--space-3);
    padding: var(--space-4);
    border-bottom: 1px solid var(--border-strong);
    background: color-mix(in srgb, var(--surface-1) 94%, transparent);
    backdrop-filter: blur(12px);
  }
  .close-detail {
    display: inline-flex;
    align-items: center;
    justify-self: start;
    gap: 6px;
    min-height: 28px;
    padding: 0 8px 0 6px;
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-2);
    color: var(--text-muted);
    font-size: var(--type-xs);
    font-weight: 700;
    cursor: pointer;
  }
  .close-detail:hover {
    border-color: var(--border-strong);
    color: var(--text);
  }
  .close-detail span {
    display: grid;
    place-items: center;
  }
  .close-detail :global(svg) {
    width: 14px;
    height: 14px;
  }
  .eyebrow {
    color: var(--brand-strong);
    font: 700 9px var(--font-mono);
    letter-spacing: var(--track-wide);
    text-transform: uppercase;
  }
  h2,
  h3,
  p {
    margin: 0;
  }
  h2 {
    margin-top: 3px;
    font-family: var(--font-display);
    font-size: clamp(18px, 2vw, 23px);
    font-weight: 650;
    letter-spacing: var(--track-tight);
  }
  h2 time {
    display: grid;
    gap: 1px;
  }
  h2 time span {
    color: var(--text-muted);
    font: 550 var(--type-sm) var(--font-mono);
  }
  .run-facts {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    border-bottom: 1px solid var(--border);
    background: linear-gradient(
      90deg,
      color-mix(in srgb, var(--brand) 6%, var(--surface-inset)),
      var(--surface-inset)
    );
    box-shadow: inset 0 -1px 0 var(--border);
  }
  .run-facts div {
    min-width: 0;
    padding: 11px var(--space-3);
  }
  .run-facts div + div {
    border-left: 1px solid var(--border);
  }
  .run-facts span,
  dt {
    color: var(--text-soft);
    font-size: 9px;
    letter-spacing: 0.03em;
  }
  .run-facts strong {
    display: block;
    margin-top: 3px;
    overflow-wrap: anywhere;
    font: 650 var(--type-xs) var(--font-mono);
  }
  .run-facts strong.partial {
    color: var(--warn);
  }
  .detail-group {
    margin: 0;
    border-bottom: 1px solid var(--border);
  }
  .section-head {
    display: grid;
    grid-template-columns: 22px minmax(0, 1fr) auto auto;
    align-items: center;
    gap: var(--space-2);
    padding: 11px var(--space-4);
    cursor: pointer;
  }
  .section-head::marker {
    color: var(--brand);
  }
  .section-head:hover {
    background: color-mix(in srgb, var(--brand) 5%, var(--surface-1));
  }
  .section-head h3 {
    font-size: var(--type-sm);
    font-weight: 750;
    letter-spacing: -0.01em;
  }
  .section-mark {
    --tone: var(--brand-strong);
    display: grid;
    width: 18px;
    height: 18px;
    place-items: center;
    color: var(--tone);
    font: 750 10px var(--font-mono);
  }
  .section-mark :global(svg) {
    width: 14px;
    height: 14px;
  }
  .section-preview {
    overflow: hidden;
    color: var(--text-soft);
    font-size: 9px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .detail-content {
    margin: 0 var(--space-4) var(--space-4);
  }
  .phase-stack {
    overflow: hidden;
  }
  .phase-row {
    --tone: var(--phase-complete);
    display: grid;
    grid-template-columns: minmax(108px, 0.65fr) minmax(0, 1.35fr);
    gap: var(--space-3);
    min-width: 0;
    padding: 12px 8px 12px 11px;
    border-left: 2px solid var(--tone);
  }
  .phase-row + .phase-row {
    border-top: 1px solid var(--border);
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
  [data-tone="latency"] {
    --tone: var(--phase-latency);
  }
  .phase-name {
    display: flex;
    align-items: center;
    gap: 7px;
    min-width: 0;
    color: var(--text-muted);
    font-size: var(--type-xs);
  }
  .phase-name span {
    display: grid;
    place-items: center;
    flex: none;
    color: var(--tone);
  }
  .phase-name :global(svg) {
    width: 15px;
    height: 15px;
  }
  .phase-reading {
    min-width: 0;
    text-align: right;
  }
  .phase-reading > strong {
    display: block;
    overflow-wrap: anywhere;
    color: var(--text);
    font: 650 clamp(13px, 1.5vw, 17px) var(--font-mono);
  }
  .phase-reading small {
    display: block;
    margin-top: 3px;
    color: var(--text-soft);
    font-size: 9px;
    line-height: 1.45;
  }
  dd {
    margin: 2px 0 0;
    overflow-wrap: anywhere;
    font: 600 var(--type-xs) var(--font-mono);
  }
  .context-list,
  .wire-list {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 0 var(--space-4);
    margin: 0;
  }
  .context-list dd,
  .wire-list dd {
    color: var(--text);
    font-size: var(--type-sm);
    line-height: 1.4;
  }
  .context-list > div,
  .wire-list > div {
    min-width: 0;
    padding: 8px 0;
    border-bottom: 1px solid var(--border-subtle);
  }
  .stage-unavailable {
    display: grid;
    gap: 4px;
    padding: 12px;
    border-left: 2px solid var(--text-soft);
    background: var(--surface-inset);
  }
  .stage-unavailable strong {
    font-size: var(--type-sm);
  }
  .stage-unavailable span {
    color: var(--text-muted);
    font-size: var(--type-xs);
    line-height: 1.45;
  }
  .issue-mark {
    color: var(--warn);
  }
  .wire-mark {
    color: var(--text-soft);
  }
  .issues ul {
    display: grid;
    gap: 6px;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .issues li {
    display: flex;
    justify-content: space-between;
    gap: var(--space-3);
    padding: 9px 10px;
    border-left: 3px solid var(--warn);
    background: var(--warn-soft);
    font-size: var(--type-xs);
    text-transform: capitalize;
  }
  .issues li span {
    color: var(--text-muted);
    text-align: right;
  }
  .wire-note {
    margin: 0 0 var(--space-3);
    color: var(--text-soft);
    font-size: 9px;
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
  @media (max-width: 560px) {
    .detail-head,
    .detail-actions {
      padding: var(--space-3);
    }
    .section-head {
      padding-inline: var(--space-3);
    }
    .detail-content {
      margin-inline: var(--space-3);
      margin-bottom: var(--space-3);
    }
    .phase-row {
      grid-template-columns: 1fr;
      gap: 6px;
    }
    .phase-reading {
      padding-left: 22px;
      text-align: left;
    }
    .context-list,
    .wire-list {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
  }
</style>
