<script lang="ts">
  import { ICON } from "../../constants";
  import {
    formatDuration,
    formatHistoryBytes,
    formatHistoryRate,
    formatLatency,
    formatPercent,
    stageStatusLabel,
  } from "../../history/format";
  import type {
    HistoryRecordV1,
    LatencyLaneSnapshot,
    ThroughputSnapshot,
  } from "../../history/types";
  import { store } from "../../state/store.svelte";

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

  const loadedProfiles = $derived<
    {
      key: "download" | "upload" | "bidirectional";
      label: string;
      icon: string;
      lane: LatencyLaneSnapshot | null;
    }[]
  >([
    {
      key: "download",
      label: "Loaded down",
      icon: ICON.download,
      lane: record.stages.latency.lanes.download,
    },
    {
      key: "upload",
      label: "Loaded up",
      icon: ICON.upload,
      lane: record.stages.latency.lanes.upload,
    },
    {
      key: "bidirectional",
      label: "Loaded bi-dir",
      icon: ICON.bidirectional,
      lane: record.stages.latency.lanes.bidirectional,
    },
  ]);
  const availableLoadedProfiles = $derived(
    loadedProfiles.filter((profile) => profile.lane !== null),
  );
  const hasResponsivenessData = $derived(
    record.stages.latency.result !== null ||
      availableLoadedProfiles.length > 0 ||
      record.bufferbloat !== null,
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
    <button class="back-detail" type="button" onclick={onClose}>
      <span>{@html ICON.back}</span>Close result
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
      <span class="disclosure" aria-hidden="true">⌄</span>
    </summary>
    <div class="detail-content phase-stack">
      <div class="phase-row" data-tone="download">
        <div class="phase-name">
          <span>{@html ICON.download}</span><strong>Download</strong>
        </div>
        <div class="phase-reading">
          <strong
            >{record.stages.download.result
              ? rate(record.stages.download.result.reportedBytesPerSec)
              : stageStatusLabel(record.stages.download.status)}</strong
          >
          {#if record.stages.download.result}<small
              >{bytes(record.stages.download.result)} transferred · {formatPercent(
                record.stages.download.result.packetLossPct,
              )} loss</small
            >{/if}
        </div>
      </div>
      <div class="phase-row" data-tone="upload">
        <div class="phase-name">
          <span>{@html ICON.upload}</span><strong>Upload</strong>
        </div>
        <div class="phase-reading">
          <strong
            >{record.stages.upload.result
              ? rate(record.stages.upload.result.reportedBytesPerSec)
              : stageStatusLabel(record.stages.upload.status)}</strong
          >
          {#if record.stages.upload.result}<small
              >{bytes(record.stages.upload.result)} transferred · {formatPercent(
                record.stages.upload.result.packetLossPct,
              )} loss</small
            >{/if}
        </div>
      </div>
      <div class="phase-row" data-tone="bidirectional">
        <div class="phase-name">
          <span>{@html ICON.bidirectional}</span><strong>Bidirectional</strong>
        </div>
        <div class="phase-reading">
          <strong
            >{record.stages.bidirectional.down && record.stages.bidirectional.up
              ? rate(
                  record.stages.bidirectional.down.reportedBytesPerSec +
                    record.stages.bidirectional.up.reportedBytesPerSec,
                )
              : stageStatusLabel(record.stages.bidirectional.status)}</strong
          >
          {#if record.stages.bidirectional.down || record.stages.bidirectional.up}
            <small>
              Down {record.stages.bidirectional.down
                ? rate(record.stages.bidirectional.down.reportedBytesPerSec)
                : "Unavailable"} · Up {record.stages.bidirectional.up
                ? rate(record.stages.bidirectional.up.reportedBytesPerSec)
                : "Unavailable"}
            </small>
          {/if}
        </div>
      </div>
    </div>
  </details>

  <details class="detail-group">
    <summary class="section-head">
      <span class="section-mark" data-tone="latency">{@html ICON.ping}</span>
      <h3 id={`result-${record.id}-latency`}>Responsiveness</h3>
      <span class="disclosure" aria-hidden="true">⌄</span>
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
        {#if record.stages.latency.result}
          <div class="idle-band" data-tone="latency">
            <div class="profile-title">
              <span>{@html ICON.ping}</span><strong>Idle</strong>
              <em>{formatLatency(record.stages.latency.result.reportedMs)}</em>
            </div>
            <dl class="metric-grid">
              <div>
                <dt>Minimum</dt>
                <dd>{formatLatency(record.stages.latency.result.minMs)}</dd>
              </div>
              <div>
                <dt>p50</dt>
                <dd>{formatLatency(record.stages.latency.result.p50Ms)}</dd>
              </div>
              <div>
                <dt>p95</dt>
                <dd>{formatLatency(record.stages.latency.result.p95Ms)}</dd>
              </div>
              <div>
                <dt>Jitter</dt>
                <dd>{formatLatency(record.stages.latency.result.jitterMs)}</dd>
              </div>
              <div>
                <dt>Loss</dt>
                <dd>
                  {formatPercent(record.stages.latency.result.packetLossPct)}
                </dd>
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
          </div>
        {:else}
          <div class="stage-unavailable compact">
            <strong>{stageStatusLabel(record.stages.latency.status)}</strong>
            <span>No idle latency summary is available.</span>
          </div>
        {/if}
        {#if availableLoadedProfiles.length}
          <div class="loaded-stack">
            {#each availableLoadedProfiles as profile (profile.key)}
              <div class="loaded-row" data-tone={profile.key}>
                <div class="profile-title">
                  <span>{@html profile.icon}</span><strong
                    >{profile.label}</strong
                  >
                  <em>{formatLatency(profile.lane!.center)}</em>
                </div>
                <dl class="metric-grid compact">
                  <div>
                    <dt>Minimum</dt>
                    <dd>{formatLatency(profile.lane!.min)}</dd>
                  </div>
                  <div>
                    <dt>p10–p90</dt>
                    <dd>
                      {formatLatency(profile.lane!.p10)} – {formatLatency(
                        profile.lane!.p90,
                      )}
                    </dd>
                  </div>
                  <div>
                    <dt>Jitter</dt>
                    <dd>{formatLatency(profile.lane!.jitter)}</dd>
                  </div>
                  <div>
                    <dt>Loss</dt>
                    <dd>{formatPercent(profile.lane!.lossRatio * 100)}</dd>
                  </div>
                </dl>
              </div>
            {/each}
          </div>
        {/if}
        {#if record.bufferbloat}
          <div class="bufferbloat-band">
            <span>Loaded latency</span>
            <strong>{formatLatency(record.bufferbloat.loadedMs)}</strong>
            <small
              >Idle {formatLatency(record.bufferbloat.idleMs)} · increase {formatLatency(
                record.bufferbloat.increaseMs,
              )} · grade {record.bufferbloat.grade}</small
            >
          </div>
        {/if}
      {/if}
    </div>
  </details>

  <details class="detail-group">
    <summary class="section-head">
      <span class="section-mark">{@html ICON.info}</span>
      <h3 id={`result-${record.id}-context`}>Run context</h3>
      <span class="section-preview">Server, transport & build</span>
      <span class="disclosure" aria-hidden="true">⌄</span>
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
        <span class="disclosure" aria-hidden="true">⌄</span>
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
        <span class="disclosure" aria-hidden="true">⌄</span>
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
  .back-detail {
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
  .back-detail:hover {
    border-color: var(--border-strong);
    color: var(--text);
  }
  .back-detail span {
    display: grid;
    place-items: center;
  }
  .back-detail :global(svg) {
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
    background: var(--surface-inset);
    box-shadow: var(--elev-recess);
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
    list-style: none;
  }
  .section-head::-webkit-details-marker {
    display: none;
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
  .disclosure {
    color: var(--text-soft);
    font: 750 13px var(--font-mono);
    transition: transform var(--dur-hover) var(--ease-out);
  }
  .detail-group[open] > .section-head .disclosure {
    transform: rotate(180deg);
  }
  .detail-content {
    margin: 0 var(--space-4) var(--space-4);
  }
  .phase-stack,
  .loaded-stack {
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
  .phase-row + .phase-row,
  .loaded-row + .loaded-row {
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
  .phase-name,
  .profile-title {
    display: flex;
    align-items: center;
    gap: 7px;
    min-width: 0;
    color: var(--text-muted);
    font-size: var(--type-xs);
  }
  .phase-name span,
  .profile-title > span {
    display: grid;
    place-items: center;
    flex: none;
    color: var(--tone);
  }
  .phase-name :global(svg),
  .profile-title :global(svg) {
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
  .idle-band,
  .loaded-row {
    --tone: var(--phase-latency);
    padding: 12px;
    border-left: 2px solid var(--tone);
    background: var(--surface-inset);
  }
  .idle-band {
    border-left: 2px solid var(--tone);
  }
  .loaded-stack {
    margin-top: var(--space-2);
  }
  .profile-title strong {
    font-weight: 700;
  }
  .profile-title em {
    margin-left: auto;
    overflow-wrap: anywhere;
    color: var(--text);
    font: 650 var(--type-sm) var(--font-mono);
    font-style: normal;
    text-align: right;
  }
  .metric-grid {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 9px var(--space-3);
    margin: var(--space-3) 0 0;
  }
  .metric-grid.compact {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }
  .metric-grid div {
    min-width: 0;
  }
  dd {
    margin: 2px 0 0;
    overflow-wrap: anywhere;
    font: 600 var(--type-xs) var(--font-mono);
  }
  .bufferbloat-band {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: 3px var(--space-3);
    margin-top: var(--space-2);
    padding: 11px 12px;
    border-top: 1px solid var(--border);
    border-bottom: 1px solid var(--border);
    background: color-mix(in srgb, var(--phase-latency) 7%, var(--surface-2));
  }
  .bufferbloat-band span {
    color: var(--text-muted);
    font-size: var(--type-xs);
    font-weight: 700;
  }
  .bufferbloat-band strong {
    color: var(--text);
    font: 650 var(--type-sm) var(--font-mono);
  }
  .bufferbloat-band small {
    grid-column: 1 / -1;
    color: var(--text-soft);
    font-size: 9px;
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
  .stage-unavailable.compact {
    margin-bottom: var(--space-2);
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
    .metric-grid,
    .metric-grid.compact,
    .context-list,
    .wire-list {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
  }
</style>
