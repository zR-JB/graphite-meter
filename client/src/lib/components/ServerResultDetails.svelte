<script lang="ts">
  import type { MultiServerResult } from "../servers/measurement";
  import { store } from "../state/store.svelte";
  import { fmtSpeed, fmtMs } from "../format";
  let {
    details,
    outcome = "complete",
  }: {
    details: MultiServerResult;
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

<details class="server-results">
  <summary
    >{outcome === "incomplete"
      ? "Measurement incomplete"
      : details.failures.some((failure) => failure.scope === "throughput")
        ? `Completed with ${details.participants.length} of ${details.selection.length} servers`
        : details.failures.length
          ? "Completed · Latency interrupted"
          : `${details.selection.length} ${details.selection.length === 1 ? "server" : "servers"}`}
    <span>Details</span></summary
  >
  <p>
    Per-server measurements were taken while sharing the connection. Earlier
    intervals remain available below.
  </p>
  <div class="table-scroll">
    <table>
      <thead
        ><tr
          ><th>Server</th><th>Download</th><th>Upload</th
          >{#if details.intervals.some((interval) => interval.stage === "bidirectional")}<th
              >Concurrent ↓ / ↑</th
            >{/if}<th>Idle latency</th></tr
        ></thead
      ><tbody>
        {#each details.servers as server (server.server.id)}<tr
            ><th scope="row"
              >{server.server.name}<small
                >{details.participants.includes(server.server.id)
                  ? server.throughput.transport
                  : "Earlier partial measurement"}</small
              ></th
            ><td
              >{rate(component(server.server.id, "download")?.bytesPerSec)}</td
            ><td>{rate(component(server.server.id, "upload")?.bytesPerSec)}</td
            >{#if details.intervals.some((interval) => interval.stage === "bidirectional")}<td
                >{rate(
                  component(server.server.id, "bidirectional", "down")
                    ?.bytesPerSec,
                )} / {rate(
                  component(server.server.id, "bidirectional", "up")
                    ?.bytesPerSec,
                )}</td
              >{/if}<td
              >{server.latency
                ? `${fmtMs(server.latency.reportedMs)} ms`
                : "—"}</td
            ></tr
          >{/each}
      </tbody>
    </table>
  </div>
  {#if details.failures.length}<ul>
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
    <summary>Measurement intervals</summary>{#if details.omittedIntervals}<p>
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
</details>

<style>
  .server-results {
    border-top: 1px solid var(--border);
    padding-top: 0.75rem;
    font-size: 0.75rem;
    color: var(--text-muted);
  }
  summary {
    cursor: pointer;
    color: var(--text);
    padding: 0.35rem 0;
  }
  summary span {
    color: var(--text-muted);
    margin-left: 0.5rem;
    font-size: 0.7rem;
  }
  p {
    line-height: 1.5;
    margin: 0.65rem 0;
  }
  .table-scroll {
    overflow: auto;
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-variant-numeric: tabular-nums;
  }
  th,
  td {
    text-align: right;
    padding: 0.6rem 0.65rem;
    border-bottom: 1px solid var(--border);
    white-space: nowrap;
  }
  th:first-child {
    text-align: left;
    padding-left: 0;
  }
  thead th {
    font-size: 0.65rem;
    font-weight: 500;
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  tbody th {
    font-weight: 500;
    color: var(--text);
  }
  small {
    display: block;
    color: var(--text-muted);
    font-size: 0.65rem;
    font-weight: 400;
    margin-top: 0.2rem;
  }
  td {
    font-family: var(--font-mono);
    color: var(--text);
  }
  ul {
    padding-left: 1rem;
  }
  li {
    margin: 0.5rem 0;
    line-height: 1.5;
  }
  .intervals {
    margin-top: 0.5rem;
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
