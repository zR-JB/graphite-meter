<script lang="ts">
  import { store } from "../state/store.svelte";
  import { fmtMs } from "../format";
  import { BUILD } from "../buildenv";
  import { describeTransferStreams } from "../runner/real/streamPolicy";

  const connections = $derived(
    store.isRunning ? store.runConnections : store.connections,
  );
  const server = $derived(
    store.transportDiscovery?.server ?? store.infra?.server,
  );
  let copied = $state(false);

  function status(state: string) {
    return state === "verified"
      ? "Ready"
      : state[0].toUpperCase() + state.slice(1);
  }

  function clientEvidence(role: "throughput" | "latency") {
    const connection = connections[role];
    if (!connection.clientIp) return "Pending";
    const source =
      connection.clientIpSource === "forwarded"
        ? "trusted proxy"
        : "socket peer";
    return `${connection.clientIp} · IPv${connection.clientIpVersion} · ${source}`;
  }

  function report() {
    return JSON.stringify(
      {
        clientVersion: BUILD.clientVersion,
        server,
        generation: store.transportDiscovery?.generation,
        throughput: connections.throughput,
        latency: connections.latency,
        preTestPingMs: store.infra?.preTestPingMs,
        streams: describeTransferStreams(
          store.runConfig.transferStreams,
          store.infra?.selectedThroughputProtocol,
        ),
        compensation: store.config.compensation,
      },
      null,
      2,
    );
  }

  async function copyReport() {
    await navigator.clipboard.writeText(report());
    copied = true;
    window.setTimeout(() => (copied = false), 1500);
  }
</script>

<section class="endpoint">
  <article class="server">
    <div>
      <span>Server</span>
      <strong>{server?.name ?? "Checking server"}</strong>
      <small>{server?.location ?? "Location unavailable"}</small>
    </div>
    <div>
      <span>Version</span>
      <strong>{store.transportDiscovery?.engineVersion ?? "—"}</strong>
      <small>Client {BUILD.clientVersion}</small>
    </div>
    <div>
      <span>Pre-test RTT</span>
      <strong
        >{store.infra ? `${fmtMs(store.infra.preTestPingMs)} ms` : "—"}</strong
      >
      <small>Selected latency path</small>
    </div>
  </article>

  <div class="paths">
    {#each ["throughput", "latency"] as role}
      {@const connection = connections[role as "throughput" | "latency"]}
      <article class="path">
        <header>
          <div>
            <span>{role}</span>
            <strong>{connection.label}</strong>
          </div>
          <mark data-state={connection.validation}
            >{status(connection.validation)}</mark
          >
        </header>
        <p>{connection.summary}</p>
        <dl>
          <div>
            <dt>Browser-facing</dt>
            <dd>{connection.browserProtocol ?? "Pending"}</dd>
          </div>
          <div>
            <dt>Server-observed</dt>
            <dd>{connection.serverProtocol ?? "Pending"}</dd>
          </div>
          <div>
            <dt>Client</dt>
            <dd>{clientEvidence(role as "throughput" | "latency")}</dd>
          </div>
        </dl>
      </article>
    {/each}
  </div>

  <details>
    <summary>Diagnostics</summary>
    <div class="diagnostics">
      <dl>
        <div>
          <dt>Generation</dt>
          <dd>{store.transportDiscovery?.generation ?? "—"}</dd>
        </div>
        <div>
          <dt>Throughput target</dt>
          <dd>
            {connections.throughput.target?.id ??
              connections.throughput.selection}
          </dd>
        </div>
        <div>
          <dt>Throughput origin</dt>
          <dd>{connections.throughput.target?.origin ?? "—"}</dd>
        </div>
        <div>
          <dt>Latency target</dt>
          <dd>
            {connections.latency.target?.id ?? connections.latency.selection}
          </dd>
        </div>
        <div>
          <dt>Latency origin</dt>
          <dd>{connections.latency.target?.origin ?? "—"}</dd>
        </div>
        <div>
          <dt>Upload progress</dt>
          <dd>Selected throughput path · NDJSON</dd>
        </div>
        <div>
          <dt>Streams</dt>
          <dd>
            {describeTransferStreams(
              store.runConfig.transferStreams,
              store.infra?.selectedThroughputProtocol,
            )}
          </dd>
        </div>
        <div>
          <dt>Compensation</dt>
          <dd>
            {store.config.compensation.profile} · {store.config.compensation
              .transport}
          </dd>
        </div>
      </dl>
      <button type="button" onclick={copyReport}
        >{copied ? "Copied" : "Copy diagnostic report"}</button
      >
      <span class="sr-status" aria-live="polite"
        >{copied ? "Diagnostic report copied" : ""}</span
      >
    </div>
  </details>
</section>

<style>
  .endpoint {
    display: grid;
    gap: 12px;
  }
  .server,
  .paths {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 190px), 1fr));
    gap: 10px;
  }
  article,
  details {
    min-width: 0;
    padding: var(--space-3);
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-inset);
    box-shadow: var(--elev-recess);
  }
  .server > div,
  header > div {
    display: grid;
    gap: 3px;
  }
  span,
  dt,
  small {
    color: var(--text-soft);
    font-size: 10px;
  }
  strong {
    color: var(--text);
    font-size: 12px;
  }
  header {
    display: flex;
    justify-content: space-between;
    gap: 8px;
  }
  header span {
    font-weight: 800;
    letter-spacing: 0.1em;
    text-transform: uppercase;
  }
  mark {
    align-self: start;
    padding: 3px 6px;
    border-radius: 999px;
    background: var(--warn-soft);
    color: var(--warn);
    font-size: 9px;
  }
  mark[data-state="verified"] {
    background: var(--ok-soft);
    color: var(--ok);
  }
  p {
    margin: 10px 0;
    color: var(--text-soft);
    font-family: var(--font-mono);
    font-size: 10px;
  }
  dl {
    display: grid;
    gap: 7px;
    margin: 0;
  }
  dl div {
    display: grid;
    grid-template-columns: minmax(90px, max-content) minmax(0, 1fr);
    gap: 10px;
  }
  dd {
    min-width: 0;
    margin: 0;
    color: var(--text);
    font-family: var(--font-mono);
    font-size: 10px;
    overflow-wrap: anywhere;
  }
  summary {
    color: var(--text);
    font-size: 12px;
    font-weight: 800;
    cursor: pointer;
  }
  .diagnostics {
    display: grid;
    gap: 12px;
    margin-top: 12px;
  }
  button {
    justify-self: start;
    min-height: 34px;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-sm);
    background: var(--surface-1);
    color: var(--text);
    padding: 6px 10px;
    cursor: pointer;
  }
  .sr-status {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0, 0, 0, 0);
  }
  @container (max-width: 300px) {
    dl div {
      grid-template-columns: 1fr;
      gap: 2px;
    }
  }
</style>
