<script lang="ts">
  import { store } from "../state/store.svelte";
  import { fmtMs } from "../format";
  import { BUILD } from "../buildenv";
  import { describeTransferStreams } from "../runner/real/streamPolicy";
  import { httpProtocolLabel } from "../runner/protocol";

  const connections = $derived(
    store.isRunning ? store.runConnections : store.connections,
  );
  const server = $derived(
    store.transportDiscovery?.server ?? store.infra?.server,
  );
  const engine = $derived(store.engineInfo);
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

  function protocolEvidence(role: "throughput" | "latency") {
    const connection = connections[role];
    if (role === "latency")
      return connection.serverProtocol
        ? `Path check reached server over ${httpProtocolLabel(connection.serverProtocol)}`
        : "Pending";
    if (!connection.browserProtocol && !connection.serverProtocol)
      return "Pending";
    return `Browser to endpoint ${httpProtocolLabel(connection.browserProtocol)} · server received ${httpProtocolLabel(connection.serverProtocol)}`;
  }

  function capability(value: string, role: "throughput" | "latency") {
    if (value === "fetch-streams") return "Fetch streams";
    if (value === "websocket") return "WebSocket";
    if (value === "webtransport-streams") return "WebTransport streams";
    if (value === "webtransport-datagrams") return "WebTransport datagrams";
    if (value === "webtransport")
      return role === "throughput"
        ? "WebTransport streams"
        : "WebTransport datagrams";
    return value;
  }

  function capabilities(role: "throughput" | "latency") {
    const values =
      role === "throughput"
        ? engine?.throughputTransports
        : engine?.latencyTransports;
    return values?.map((value) => capability(value, role)).join(" · ") ?? "—";
  }

  const uploadProgressPath = $derived(
    connections.throughput.target
      ? `Fetch streams over ${connections.throughput.summary.replace("Fetch stream · ", "")}`
      : "Pending",
  );
  const serverInstance = $derived.by(() => {
    const value = store.transportDiscovery?.generation;
    if (!value || value === "dummy") return value ?? "—";
    return `${value.slice(0, 8)}…`;
  });

  function report() {
    return JSON.stringify(
      {
        clientVersion: BUILD.clientVersion,
        server,
        generation: store.transportDiscovery?.generation,
        throughput: connections.throughput,
        latency: connections.latency,
        preTestPingMs: connections.latency.preTestPingMs,
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

<section class="infra">
  <div class="grid">
    <article class="card">
      <h3>Server</h3>
      <dl>
        <div>
          <dt>Node</dt>
          <dd>{server?.name ?? "Checking server"}</dd>
        </div>
        <div>
          <dt>Location</dt>
          <dd>{server?.location ?? "Unavailable"}</dd>
        </div>
        <div>
          <dt>Version</dt>
          <dd>{store.transportDiscovery?.engineVersion ?? "—"}</dd>
        </div>
        <div>
          <dt>Client</dt>
          <dd>{BUILD.clientVersion}</dd>
        </div>
      </dl>
    </article>

    <article class="card">
      <h3>Measurement engine</h3>
      <dl>
        <div>
          <dt>Runner</dt>
          <dd>{engine?.name ?? "—"} · {engine?.version ?? "—"}</dd>
        </div>
        <div>
          <dt>Throughput</dt>
          <dd>{capabilities("throughput")}</dd>
        </div>
        <div>
          <dt>Latency</dt>
          <dd>{capabilities("latency")}</dd>
        </div>
      </dl>
    </article>

    {#each ["throughput", "latency"] as role}
      {@const typedRole = role as "throughput" | "latency"}
      {@const connection = connections[typedRole]}
      <article class="card path">
        <header>
          <h3>{role} path</h3>
          <mark data-state={connection.validation}
            >{status(connection.validation)}</mark
          >
        </header>
        <dl>
          <div>
            <dt>Selected</dt>
            <dd>{connection.summary}</dd>
          </div>
          <div>
            <dt>
              {typedRole === "throughput" ? "Observed HTTP" : "Path check"}
            </dt>
            <dd>{protocolEvidence(typedRole)}</dd>
          </div>
          <div>
            <dt>Client</dt>
            <dd>{clientEvidence(typedRole)}</dd>
          </div>
          {#if typedRole === "throughput"}
            <div>
              <dt>Upload progress</dt>
              <dd>{uploadProgressPath}</dd>
            </div>
          {:else}
            <div>
              <dt>Pre-test RTT</dt>
              <dd>
                {connection.preTestPingMs !== undefined
                  ? `${fmtMs(connection.preTestPingMs)} ms`
                  : "Pending"}
              </dd>
            </div>
          {/if}
        </dl>
      </article>
    {/each}
  </div>

  <details class="diagnostics-card">
    <summary>Diagnostics</summary>
    <div class="diagnostics">
      <dl>
        <div>
          <dt>Server instance</dt>
          <dd title={store.transportDiscovery?.generation ?? undefined}>
            {serverInstance}
          </dd>
        </div>
        <div>
          <dt>Throughput origin</dt>
          <dd>{connections.throughput.target?.origin ?? "—"}</dd>
        </div>
        <div>
          <dt>Latency origin</dt>
          <dd>{connections.latency.target?.origin ?? "—"}</dd>
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
      <p class="diagnostic-note">
        Server instance changes when the backend restarts. Browser protocol
        describes the selected endpoint; server protocol is what its path check
        delivered to Graphite Meter.
      </p>
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
  .infra {
    display: grid;
    gap: 14px;
    container-type: inline-size;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 240px), 1fr));
    gap: 12px;
  }
  .card,
  .diagnostics-card {
    position: relative;
    display: grid;
    align-content: start;
    gap: 10px;
    min-width: 0;
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background:
      linear-gradient(180deg, var(--surface-2), transparent),
      var(--surface-inset);
    padding: var(--space-3);
    box-shadow: var(--elev-recess);
    overflow: clip;
  }
  .card > * {
    position: relative;
    z-index: 1;
  }
  h3 {
    margin: 0;
    color: var(--text-soft);
    font-size: 10px;
    font-weight: 850;
    letter-spacing: 0.13em;
    text-transform: uppercase;
  }
  header {
    display: flex;
    justify-content: space-between;
    gap: 8px;
  }
  mark {
    align-self: start;
    padding: 3px 6px;
    border-radius: 999px;
    background: var(--warn-soft);
    color: var(--warn);
    font-size: 9px;
    font-weight: 700;
  }
  mark[data-state="verified"] {
    background: var(--ok-soft);
    color: var(--ok);
  }
  dl {
    display: grid;
    gap: 7px;
    margin: 0;
  }
  dl div {
    display: grid;
    grid-template-columns: minmax(90px, max-content) minmax(0, 1fr);
    gap: 12px;
    align-items: baseline;
  }
  dt {
    color: var(--text-soft);
    font-size: 11px;
    font-weight: 700;
  }
  dd {
    min-width: 0;
    margin: 0;
    color: var(--text);
    font-family: var(--font-mono);
    font-size: 11px;
    overflow-wrap: anywhere;
  }
  .diagnostics-card {
    padding: 0;
  }
  .diagnostics-card summary {
    cursor: pointer;
    padding: var(--space-3);
    color: var(--text-soft);
    font-size: 10px;
    font-weight: 850;
    letter-spacing: 0.13em;
    text-transform: uppercase;
  }
  .diagnostics-card[open] summary {
    border-bottom: 1px solid var(--border);
  }
  .diagnostics-card summary:hover {
    color: var(--text);
  }
  /* Full-bleed against a clipping card, so the ring goes inside the edge. */
  .diagnostics-card summary:focus-visible {
    outline: var(--focus-ring);
    outline-offset: -2px;
  }
  .diagnostics {
    display: grid;
    gap: 12px;
    padding: var(--space-3);
  }
  .diagnostic-note {
    margin: 0;
    color: var(--text-soft);
    font-size: 10px;
    line-height: 1.5;
  }
  button {
    justify-self: start;
    min-height: 34px;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-sm);
    background: var(--surface-1);
    color: var(--text);
    padding: 6px 10px;
    font-family: var(--font-sans);
    font-size: 11px;
    font-weight: 700;
    cursor: pointer;
    transition:
      border-color var(--dur-hover) var(--ease-out),
      color var(--dur-hover) var(--ease-out);
  }
  button:hover {
    border-color: color-mix(in srgb, var(--brand) 45%, var(--border-strong));
    color: var(--brand-strong);
  }
  button:focus-visible {
    outline: var(--focus-ring);
    outline-offset: 2px;
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
