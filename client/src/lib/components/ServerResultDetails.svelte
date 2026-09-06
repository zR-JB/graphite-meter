<script lang="ts">
  import type { MultiServerResult } from "../servers/measurement";
  import { store } from "../state/store.svelte";
  import { fmtSpeed, fmtMs } from "../format";
  let {
    details,
    outcome = "complete",
    embedded = false,
  }: {
    details: MultiServerResult;
    embedded?: boolean;
    outcome?: "complete" | "partial" | "incomplete";
  } = $props();
  function component(
    id: string,
    stage: "download" | "upload" | "bidirectional",
    dir?: "down" | "up",
  ) {
    const interval = details.intervals.findLast(
      (interval) =>
        interval.stage === stage && interval.participants.includes(id),
    );
    const window = interval?.headline ?? interval?.full;
    return (
      (dir ?? (stage === "download" ? "down" : "up")) === "down"
        ? window?.down
        : window?.up
    )?.find((component) => component.serverId === id);
  }
  function rate(value: number | undefined) {
    return value === undefined
      ? "—"
      : `${fmtSpeed(store.toUnit(value))} ${store.unitLabel}`;
  }
</script>

<section class="server-results" class:embedded aria-label="Per-server results">
  <header>
    <h3>Per-server results</h3>
    <p class="result-status">
      {outcome === "incomplete"
        ? "Measurement incomplete"
        : details.failures.some((failure) => failure.scope === "throughput")
          ? `Completed with ${details.participants.length} of ${details.selection.length} servers`
          : details.failures.length
            ? "Completed · Latency interrupted"
            : `${details.selection.length} ${details.selection.length === 1 ? "server" : "servers"}`}
    </p>
  </header>
  <p>Contributions measured while these servers shared your connection.</p>
  <ul class="server-measurements">
    {#each details.servers as server (server.server.id)}
      <li class="server-result-row">
        <div class="server-identity">
          <strong>{server.server.name}</strong>
          <small
            >{details.participants.includes(server.server.id)
              ? server.server.location || new URL(server.server.url).host
              : "Earlier partial measurement"}</small
          >
        </div>
        <dl>
          {#if details.intervals.some((interval) => interval.stage === "download")}
            <div>
              <dt>Download</dt>
              <dd>
                {rate(component(server.server.id, "download")?.bytesPerSec)}
              </dd>
            </div>
          {/if}
          {#if details.intervals.some((interval) => interval.stage === "upload")}
            <div>
              <dt>Upload</dt>
              <dd>
                {rate(component(server.server.id, "upload")?.bytesPerSec)}
              </dd>
            </div>
          {/if}
          {#if details.intervals.some((interval) => interval.stage === "bidirectional")}
            <div>
              <dt>Concurrent ↓</dt>
              <dd>
                {rate(
                  component(server.server.id, "bidirectional", "down")
                    ?.bytesPerSec,
                )}
              </dd>
            </div>
            <div>
              <dt>Concurrent ↑</dt>
              <dd>
                {rate(
                  component(server.server.id, "bidirectional", "up")
                    ?.bytesPerSec,
                )}
              </dd>
            </div>
          {/if}
          <div>
            <dt>Idle latency</dt>
            <dd>
              {server.latency ? `${fmtMs(server.latency.reportedMs)} ms` : "—"}
            </dd>
          </div>
        </dl>
      </li>
    {/each}
  </ul>
  {#if details.failures.length}<ul class="issues">
      {#each details.failures as failure}<li>
          <strong
            >{details.selection.find((server) => server.id === failure.serverId)
              ?.name ?? failure.serverId}</strong
          >
          · {failure.stage}{failure.scope === "latency" ? " latency" : ""} · {(
            failure.atMs / 1000
          ).toFixed(1)} s — {failure.message}
        </li>{/each}
    </ul>{/if}
  <details class="intervals">
    <summary>Timing and interruptions</summary>{#if details.omittedIntervals}<p>
        {details.omittedIntervals} older intervals omitted after repeated interruptions.
        Per-server byte totals include the whole measurement.
      </p>{/if}{#each details.intervals as interval}<div class="interval">
        <strong
          >{interval.stage} · {(interval.startMs / 1000).toFixed(1)}–{(
            interval.endMs / 1000
          ).toFixed(1)} s</strong
        >
        <p>
          {interval.participants
            .map(
              (id) =>
                details.selection.find((server) => server.id === id)?.name ??
                id,
            )
            .join(", ") || "No surviving servers"} · {interval.complete
            ? "Measurement window available"
            : "Incomplete evidence"}
        </p>
        {#if interval.full}<p>
            Download {rate(interval.full.downBytesPerSec ?? undefined)} · Upload {rate(
              interval.full.upBytesPerSec ?? undefined,
            )}
          </p>
          {#if interval.full.up}<p>
              Receiver windows: {interval.full.up
                .map(
                  (window) =>
                    `${details.selection.find((server) => server.id === window.serverId)?.name}: ${(window.durationMs / 1000).toFixed(3)} s`,
                )
                .join(" · ")}
            </p>{/if}{/if}
      </div>{/each}
  </details>
</section>

<style>
  .server-results {
    min-width: 0;
    padding: var(--space-4);
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-1);
    font: 400 var(--type-sm)/1.5 var(--font-sans);
    color: var(--text-muted);
  }
  summary {
    cursor: pointer;
    color: var(--text);
    padding: 0.35rem 0;
  }
  .server-results.embedded {
    border: 0;
    border-bottom: 1px solid var(--border);
    border-radius: 0;
    background: transparent;
  }
  header {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: var(--space-3);
    flex-wrap: wrap;
  }
  h3 {
    margin: 0;
    font: 600 var(--type-md)/1.4 var(--font-sans);
    color: var(--text);
  }
  .result-status {
    margin: 0;
  }
  .server-results > p {
    margin: var(--space-2) 0 var(--space-3);
  }
  p {
    line-height: 1.5;
    margin: 0.65rem 0;
  }
  .server-measurements {
    list-style: none;
    padding: 0;
    margin: 0;
  }
  .server-result-row {
    display: grid;
    grid-template-columns: minmax(100px, 1fr) minmax(0, 3fr);
    align-items: center;
    gap: var(--space-4);
    padding: var(--space-4) 0;
    margin: 0;
    border-top: 1px solid var(--border);
  }
  .server-identity {
    display: grid;
    gap: 3px;
    min-width: 0;
  }
  .server-identity strong {
    color: var(--text);
    font-size: var(--type-sm);
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .server-identity small {
    font-size: var(--type-xs);
    overflow-wrap: anywhere;
  }
  dl {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(96px, 1fr));
    gap: var(--space-3);
    margin: 0;
  }
  dt {
    color: var(--text-muted);
    font-size: var(--type-xs);
  }
  dd {
    color: var(--text);
    font: 500 var(--type-sm)/1.5 var(--font-mono);
    font-variant-numeric: tabular-nums;
    margin: 3px 0 0;
  }
  .server-results {
    container-type: inline-size;
  }
  @container (max-width: 480px) {
    .server-result-row {
      grid-template-columns: 1fr;
      gap: var(--space-3);
    }
  }
  .issues {
    padding-left: 1rem;
  }
  .issues li {
    margin: 0.5rem 0;
    line-height: 1.5;
  }
  .intervals {
    margin-top: var(--space-3);
  }
  .interval {
    padding: 0.5rem 0;
    border-bottom: 1px solid var(--border);
  }
  .interval p {
    margin: 0.2rem 0;
    font-size: 0.7rem;
  }
  summary:focus-visible {
    outline: 2px solid var(--brand);
    outline-offset: 3px;
  }
</style>
