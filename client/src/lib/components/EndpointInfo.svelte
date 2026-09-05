<script lang="ts">
  import { store } from "../state/store.svelte";
  import { fmtMs } from "../format";
  import { BUILD } from "../buildenv";
  import { describeTransferStreams } from "../runner/real/streamPolicy";
  import { buildSegments } from "../runner/schedule";
  import {
    advertisedServerCapabilities,
    advertisedServerHttpPaths,
    pathEvidence,
    serverLoadSummary,
    endpointPathStatus,
  } from "./endpointInfo";
  import type { TransportKind } from "../runner/contract";

  type PathRole = "throughput" | "latency";
  const PATH_ROLES = ["throughput", "latency"] as const;

  // A completed result, chart, and endpoint description must identify the
  // same run. Configuration remains editable after completion, so retain its
  // frozen connection evidence until the next run begins.
  const discovery = $derived(
    store.activePaths?.discovery ?? store.transportDiscovery,
  );
  const connections = $derived(store.runConnections);
  const server = $derived(discovery?.server);
  const engine = $derived(store.engineInfo);
  let copied = $state(false);

  const pathMode = $derived(
    store.isRunning ? "running" : store.activePaths ? "result" : "live",
  );

  function clientEvidence(role: PathRole) {
    const connection = connections[role];
    if (!connection.clientIp) return "Pending";
    const source =
      connection.clientIpSource === "forwarded"
        ? "trusted proxy"
        : "socket peer";
    return `${connection.clientIp} · IPv${connection.clientIpVersion} · ${source}`;
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
    const advertised = advertisedServerCapabilities(discovery, role);
    if (!advertised) return "Checking server";
    const values = advertised.transports.map((value) =>
      capability(value, role),
    );
    if (!values.length) return "None advertised";
    return `${values.join(" · ")}${
      advertised.browserBlocked
        ? " · some clear origins blocked by this page"
        : ""
    }`;
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
    const value = discovery?.generation;
    if (!value) return "—";
    return `${value.slice(0, 8)}…`;
  });
  const serverLoad = $derived(
    serverLoadSummary(
      (
        store.activePaths?.throughput ??
        store.connectionValidation.throughput.path
      )?.probe.load,
    ),
  );
  const httpPaths = $derived(advertisedServerHttpPaths(discovery));
  let copyError = $state(false);

  // Every row here reads the same presentation the path cards do. verified path evidence
  // is the last probe's evidence, which outlives the selection that produced it
  // and is never cleared: reading it directly makes the drawer contradict the
  // card four lines above whenever a role is failed, checking, or moved.
  const throughputTransport = $derived(
    connections.throughput.target?.transport,
  );
  // The lanes a stage opens depend on what it carries, so the run's own
  // timeline supplies the stages the count is resolved from.
  const transferStreams = $derived(
    describeTransferStreams(
      store.runConfig.transferStreams,
      buildSegments(store.runConfig).segments.map(
        (segment) => segment.activity,
      ),
      connections.throughput.observedProtocol ??
        connections.throughput.target?.protocol,
      throughputTransport,
    ),
  );

  function diagnosticReport() {
    return JSON.stringify(
      {
        client: BUILD,
        server,
        generation: discovery?.generation,
        throughput: connections.throughput,
        latency: connections.latency,
        preTestPingMs: connections.latency.preTestPingMs,
        streams: transferStreams,
      },
      null,
      2,
    );
  }

  async function copyReport() {
    copyError = false;
    try {
      await navigator.clipboard.writeText(diagnosticReport());
      copied = true;
      window.setTimeout(() => (copied = false), 1500);
    } catch {
      copied = false;
      copyError = true;
    }
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
      </dl>
    </article>

    <article class="card">
      <h3>Server capabilities</h3>
      <dl>
        <div>
          <dt>HTTP versions</dt>
          {#if httpPaths === null}
            <dd>Checking server</dd>
          {:else if !httpPaths.length}
            <dd>None advertised</dd>
          {:else}
            <dd class="protocols" aria-label={httpPaths.join(" · ")}>
              {#each httpPaths as path}
                <span class="protocol">{path}</span>
              {/each}
            </dd>
          {/if}
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
      {@const status = endpointPathStatus(connection.validation, pathMode)}
      <article class="card path">
        <header>
          <h3>{role} path</h3>
          <mark data-state={status.tone}>{status.label}</mark>
        </header>
        <dl>
          <div>
            <dt>Selected</dt>
            <dd>{connection.summary}</dd>
          </div>
          <div>
            <dt>Path evidence</dt>
            <dd>
              {pathEvidence(
                role,
                connection.browserProtocol,
                connection.serverProtocol,
              )}
            </dd>
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
          <dd title={discovery?.generation ?? undefined}>
            {serverInstance}
          </dd>
        </div>
        <div>
          <dt>Server version</dt>
          <dd>{discovery?.engineVersion ?? "—"}</dd>
        </div>
        <div>
          <dt>Runner</dt>
          <dd>{engine?.name ?? "—"}</dd>
        </div>
        <div>
          <dt>Client version</dt>
          <dd>{BUILD.version ? `v${BUILD.version}` : "—"}</dd>
        </div>
        <div>
          <dt>Build profile</dt>
          <dd>{BUILD.profile}</dd>
        </div>
        <div>
          <dt>Source revision</dt>
          <dd>{BUILD.revision}</dd>
        </div>
        <div>
          <dt>Throughput origin</dt>
          <dd>{connections.throughput.target?.origin ?? "—"}</dd>
        </div>
        <div>
          <dt>Throughput client</dt>
          <dd>{clientEvidence("throughput")}</dd>
        </div>
        {#if serverLoad}
          <div>
            <dt>Server load</dt>
            <dd>{serverLoad}</dd>
          </div>
        {/if}
        <div>
          <dt>Latency origin</dt>
          <dd>{connections.latency.target?.origin ?? "—"}</dd>
        </div>
        <div>
          <dt>Latency client</dt>
          <dd>{clientEvidence("latency")}</dd>
        </div>
        <div>
          <dt>Streams</dt>
          <dd>{transferStreams}</dd>
        </div>
      </dl>
      <p class="diagnostic-note">
        Server instance changes when the backend restarts. Path evidence names
        browser and server observations only when that path exposes them.
      </p>
      <button type="button" onclick={copyReport}
        >{copied ? "Copied" : "Copy diagnostic report"}</button
      >
      <span class="sr-status" aria-live="polite"
        >{copied
          ? "Diagnostic report copied"
          : copyError
            ? "Unable to copy diagnostic report"
            : ""}</span
      >
    </div>
  </details>
</section>

<style>
  .infra {
    display: grid;
    gap: 14px;
  }
  .grid {
    display: grid;
    --endpoint-card-min: 240px;
    grid-template-columns: repeat(
      auto-fit,
      minmax(min(100%, var(--endpoint-card-min)), 1fr)
    );
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
    container-type: inline-size;
    container-name: endpoint-card;
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
    flex-wrap: wrap;
    justify-content: space-between;
    align-items: flex-start;
    gap: var(--space-2);
    min-width: 0;
  }
  header h3 {
    min-width: 0;
  }
  mark {
    flex: none;
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
  mark[data-state="ready"] {
    background: var(--ok-soft);
    color: var(--ok);
  }
  mark[data-state="active"] {
    background: var(--brand-soft);
    color: var(--brand-strong);
  }
  mark[data-state="used"] {
    background: var(--surface-2);
    color: var(--text-soft);
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
    min-width: 0;
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
    word-break: normal;
    line-height: 1.43;
  }
  .protocols {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    font-family: var(--font-sans);
    line-height: 1.2;
  }
  .protocol {
    border: 1px solid color-mix(in srgb, var(--brand) 30%, var(--border));
    border-radius: var(--r-full);
    background: color-mix(in srgb, var(--brand-soft) 72%, var(--surface-inset));
    color: var(--text);
    padding: 3px 6px;
    font-size: 9px;
    font-weight: 700;
    white-space: nowrap;
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
  @container endpoint-card (max-width: 300px) {
    dl div {
      grid-template-columns: 1fr;
      gap: 2px;
    }
  }
</style>
