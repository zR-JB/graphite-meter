<script lang="ts">
  import { store } from "../state/store.svelte";
  import { fmtMs } from "../format";
  import { BUILD } from "../buildenv";
  import { describeTransferStreams } from "../runner/real/streamPolicy";
  import { httpProtocolLabel } from "../runner/protocol";
  import type { TransportKind } from "../runner/contract";

  type PathRole = "throughput" | "latency";
  const PATH_ROLES = ["throughput", "latency"] as const;

  const connections = $derived(
    store.isRunning ? store.runConnections : store.connections,
  );
  const server = $derived(
    store.transportDiscovery?.server ?? store.infra?.server,
  );
  const engine = $derived(store.engineInfo);
  let copied = $state(false);

  function statusLabel(validation: string) {
    return validation === "verified"
      ? "Ready"
      : validation[0].toUpperCase() + validation.slice(1);
  }

  function clientEvidence(role: PathRole) {
    const connection = connections[role];
    if (!connection.clientIp) return "Pending";
    const source =
      connection.clientIpSource === "forwarded"
        ? "trusted proxy"
        : "socket peer";
    return `${connection.clientIp} · IPv${connection.clientIpVersion} · ${source}`;
  }

  function protocolEvidence(role: PathRole) {
    const connection = connections[role];
    if (role === "latency")
      return connection.serverProtocol
        ? `Path check reached server over ${httpProtocolLabel(connection.serverProtocol)}`
        : "Pending";
    if (!connection.browserProtocol && !connection.serverProtocol)
      return "Pending";
    return `Browser to endpoint ${httpProtocolLabel(connection.browserProtocol)} · server received ${httpProtocolLabel(connection.serverProtocol)}`;
  }

  // One vocabulary throughout: TransportKind. Bare "webtransport" names the
  // session, whose throughput half is streams and whose latency half is the
  // datagram bus.
  function capability(value: TransportKind, role: PathRole) {
    const labels: Record<TransportKind, string> = {
      "fetch-stream": "Fetch streams",
      websocket: "WebSocket",
      "webtransport-datagram": "WebTransport datagrams",
      webtransport:
        role === "throughput"
          ? "WebTransport streams"
          : "WebTransport datagrams",
    };
    return labels[value] ?? value;
  }

  function capabilities(role: PathRole) {
    const values =
      role === "throughput"
        ? engine?.throughputTransports
        : engine?.latencyTransports;
    return values?.map((value) => capability(value, role)).join(" · ") ?? "—";
  }

  // The feed rides the session that carries the bytes, or its own fetch when
  // the lanes are fetches.
  const uploadProgressPath = $derived.by(() => {
    const target = connections.throughput.target;
    if (!target) return "Pending";
    const carrier =
      target.transport === "fetch-stream" ? "Fetch stream" : "Session stream";
    // The summary opens with its own mechanism, so naming the carrier again
    // would stutter: what is left is the path it runs over.
    const over = connections.throughput.summary
      .split(" · ")
      .slice(1)
      .join(" · ");
    return `${carrier} over ${over}`;
  });
  const serverInstance = $derived.by(() => {
    const value = store.transportDiscovery?.generation;
    if (!value || value === "dummy") return value ?? "—";
    return `${value.slice(0, 8)}…`;
  });
  // Concurrent tests contend for bandwidth and CPU; past half occupancy the
  // caution tells the user their numbers may reflect the neighbors.
  const serverLoad = $derived.by(() => {
    const load = store.infra?.serverLoad;
    if (!load) return null;
    const busy = load.max > 0 && load.active / load.max >= 0.5;
    return {
      text: `${load.active} of ${load.max} slots`,
      caution: busy ? "server busy — results may be affected" : null,
    };
  });

  function diagnosticReport() {
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
          store.infra?.selectedThroughputTransport,
        ),
        compensation: store.config.compensation,
      },
      null,
      2,
    );
  }

  async function copyReport() {
    await navigator.clipboard.writeText(diagnosticReport());
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

    {#each PATH_ROLES as role}
      {@const connection = connections[role]}
      <article class="card path">
        <header>
          <h3>{role} path</h3>
          <mark data-state={connection.validation}
            >{statusLabel(connection.validation)}</mark
          >
        </header>
        <dl>
          <div>
            <dt>Selected</dt>
            <dd>{connection.summary}</dd>
          </div>
          <div>
            <dt>
              {role === "throughput" ? "Observed HTTP" : "Path check"}
            </dt>
            <dd>{protocolEvidence(role)}</dd>
          </div>
          <div>
            <dt>Client</dt>
            <dd>{clientEvidence(role)}</dd>
          </div>
          {#if role === "throughput"}
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
          <dt>Transports</dt>
          <dd>
            {store.infra?.selectedThroughputTransport ?? "—"} · {store.infra
              ?.selectedLatencyTransport ?? "—"}
          </dd>
        </div>
        {#if serverLoad}
          <div>
            <dt>Server load</dt>
            <dd>
              {serverLoad.text}{#if serverLoad.caution}
                · {serverLoad.caution}{/if}
            </dd>
          </div>
        {/if}
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
              store.infra?.selectedThroughputTransport,
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
    gap: var(--space-3);
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
    gap: var(--space-2);
  }
  mark {
    align-self: start;
    padding: 3px 6px;
    border-radius: var(--r-full);
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
    gap: var(--space-3);
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
    gap: var(--space-3);
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
    border-radius: var(--r-chrome);
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
