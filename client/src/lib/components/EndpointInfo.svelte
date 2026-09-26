<script lang="ts">
  import { catalogSelection } from "../presentation/serverAppearance";
  import {
    describeTransferStreams,
    emptyConnectionValidation,
    latencyPathNeeded,
  } from "../runner/paths";
  import { presentConnections } from "../presentation/paths";
  import { store } from "../state/store.svelte";
  import { fmtMs } from "../format";
  import { BUILD } from "../buildenv";
  import { buildSegments } from "../runner/schedule";
  import {
    advertisedServerCapabilities,
    advertisedServerHttpPaths,
    pathEvidence,
    serverLoadSummary,
    endpointPathStatus,
  } from "./endpointInfo";
  import { MISSING, transportLabel } from "../presentation/vocabulary";
  import ServerScope from "./ServerScope.svelte";

  type PathRole = "throughput" | "latency";
  const PATH_ROLES = ["throughput", "latency"] as const;
  const BADGE_TONE: Partial<Record<string, string>> = {
    verified: "ok",
    ready: "ok",
    active: "brand",
    used: "neutral",
  };

  let { onOpenLegal }: { onOpenLegal: (invoker: HTMLElement) => void } =
    $props();
  let inspectedServer = $state("");
  const availableServers = $derived(
    store.activeServers.length
      ? store.activeServers.map((entry) => entry.server)
      : catalogSelection(store.serverCatalog, store.selectedServers),
  );
  const selectedServer = $derived(
    availableServers.find((server) => server.id === inspectedServer) ??
      availableServers.find((server) => server.id === store.latencyFocus) ??
      availableServers[0],
  );
  const captured = $derived(
    store.activeServers.find((entry) => entry.server.id === selectedServer?.id),
  );
  // Inspection never changes selection; completed runs keep their prepared paths.
  const activePaths = $derived(
    captured?.paths ?? (store.activeServers.length ? null : store.activePaths),
  );
  const discovery = $derived(
    activePaths?.discovery ??
      (selectedServer
        ? (store.servers.get(selectedServer.id)?.discovery ?? null)
        : store.transportDiscovery),
  );
  const validation = $derived(
    selectedServer
      ? (store.servers.get(selectedServer.id)?.validation ??
          emptyConnectionValidation())
      : store.connectionValidation,
  );
  const connections = $derived(
    presentConnections(store.runConfig, discovery, validation, activePaths),
  );
  const server = $derived(discovery?.server ?? selectedServer);
  const latencyRequested = $derived(
    activePaths
      ? activePaths.latency !== null
      : latencyPathNeeded(store.config) &&
          (store.latencySelection.mode === "all" ||
            selectedServer?.id === store.primaryLatencyServer),
  );
  const failures = $derived(
    store.serverDetails?.failures.filter(
      (failure) => failure.serverId === selectedServer?.id,
    ) ?? [],
  );
  const engine = $derived(store.engineInfo);
  let copied = $state(false);

  const pathMode = $derived(
    store.isRunning ? "running" : activePaths ? "result" : "live",
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

  function capabilities(role: PathRole) {
    const advertised = advertisedServerCapabilities(discovery, role);
    if (!advertised) return "Checking server";
    const values = advertised.transports.map((value) =>
      transportLabel(value, role),
    );
    if (!values.length) return "None advertised";
    return `${values.join(" · ")}${
      advertised.browserBlocked
        ? " · some clear origins blocked by this page"
        : ""
    }`;
  }

  // The feed rides the session carrying the bytes, or its own fetch.
  const uploadProgressPath = $derived.by(() => {
    const target = connections.throughput.target;
    if (!target) return "Pending";
    const carrier =
      target.transport === "fetch-stream" ? "Fetch stream" : "Session stream";
    const over = connections.throughput.summary
      .split(" · ")
      .slice(1)
      .join(" · ");
    return `${carrier} over ${over}`;
  });
  const serverLoad = $derived(
    serverLoadSummary(
      (activePaths?.throughput ?? validation.throughput.path)?.probe.load,
    ),
  );
  const httpPaths = $derived(advertisedServerHttpPaths(discovery));
  let copyError = $state(false);

  // Rows read the path cards' presentation, never stale verified evidence.
  const throughputTransport = $derived(
    connections.throughput.target?.transport,
  );
  // The run's own timeline decides which stages resolve the stream count.
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
        scope: pathMode,
        selectedServers: availableServers.map(({ id, name, url }) => ({
          id,
          name,
          url,
        })),
        failures,
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
    <article class="surface-inset card server-card">
      <header>
        <h3 class="caps">
          {pathMode === "live" ? "Selected servers" : "Tested servers"}
        </h3>
        <span class="hint"
          >{availableServers.length > 1
            ? `${availableServers.length} servers`
            : "Single server"}</span
        >
      </header>
      {#if availableServers.length > 1}
        <ServerScope
          servers={availableServers}
          value={selectedServer?.id ?? ""}
          label="Inspect server"
          onchange={(id) => (inspectedServer = id)}
        />
      {/if}
      <dl class="kv">
        <div>
          <dt>Node</dt>
          <dd>{server?.name ?? "Checking server"}</dd>
        </div>
        <div>
          <dt>Location</dt>
          <dd>{server?.location ?? "Unavailable"}</dd>
        </div>
        {#if selectedServer}
          <div>
            <dt>Address</dt>
            <dd>{selectedServer.url}</dd>
          </div>
        {/if}
      </dl>
      <p class="hint">
        {pathMode === "live"
          ? "Current selection and verified connections."
          : pathMode === "running"
            ? "Connections used by this test."
            : "Connections captured for the displayed result."}
      </p>
      {#each failures as failure}
        <p class="endpoint-failure">{failure.message}</p>
      {/each}
    </article>

    {#each PATH_ROLES as role}
      {@const connection = connections[role]}
      {@const status = endpointPathStatus(connection.validation, pathMode)}
      <article class="surface-inset card path">
        <header>
          <h3 class="caps">{role} path</h3>
          <span
            class="badge"
            data-tone={BADGE_TONE[
              role === "latency" && !latencyRequested ? "used" : status.tone
            ] ?? "warn"}
            >{role === "latency" && !latencyRequested
              ? pathMode === "live"
                ? "Not selected"
                : "Not in test"
              : status.label}</span
          >
        </header>
        <dl class="kv">
          <div>
            <dt>Selected</dt>
            <dd>
              {role === "latency" && !latencyRequested
                ? "Not selected for latency measurement"
                : connection.summary}
            </dd>
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
              <dt>Pre-test latency</dt>
              <dd>
                {connection.preTestPingMs !== undefined
                  ? `${fmtMs(connection.preTestPingMs)} ms`
                  : latencyRequested
                    ? "Pending"
                    : MISSING}
              </dd>
            </div>
          {/if}
        </dl>
      </article>
    {/each}
  </div>

  <details class="surface-inset card disclosure capabilities-card">
    <summary>Server capabilities</summary>
    <dl class="kv">
      <div>
        <dt>HTTP versions</dt>
        {#if httpPaths === null}
          <dd>Checking server</dd>
        {:else if !httpPaths.length}
          <dd>None advertised</dd>
        {:else}
          <dd class="protocols" aria-label={httpPaths.join(" · ")}>
            {#each httpPaths as path}
              <span class="badge" data-tone="brand">{path}</span>
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
  </details>

  <details class="surface-inset disclosure diagnostics-card">
    <summary>Diagnostics</summary>
    <div class="diagnostics">
      <dl class="kv">
        <div>
          <dt>Server instance</dt>
          <dd>{discovery?.generation || MISSING}</dd>
        </div>
        <div>
          <dt>Server version</dt>
          <dd>{discovery?.engineVersion ?? MISSING}</dd>
        </div>
        <div>
          <dt>Runner</dt>
          <dd>{engine?.name ?? MISSING}</dd>
        </div>
        <div>
          <dt>Client version</dt>
          <dd>{BUILD.version ? `v${BUILD.version}` : MISSING}</dd>
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
          <dd>{connections.throughput.target?.origin ?? MISSING}</dd>
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
          <dd>{connections.latency.target?.origin ?? MISSING}</dd>
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
      <p class="hint">
        Server instance changes when the backend restarts. Path evidence names
        browser and server observations only when that path exposes them.
      </p>
      <button class="btn" type="button" onclick={copyReport}
        >{copied ? "Copied" : "Copy diagnostic report"}</button
      >
      <span class="sr-only" aria-live="polite"
        >{copied
          ? "Diagnostic report copied"
          : copyError
            ? "Unable to copy diagnostic report"
            : ""}</span
      >
    </div>
  </details>
  <p class="license">
    <span>Legal</span>
    <button
      class="btn-link"
      type="button"
      onclick={(event) => onOpenLegal(event.currentTarget)}
      >About &amp; legal</button
    >
  </p>
</section>

<style>
  .infra,
  .grid {
    display: grid;
    gap: var(--space-3);
  }
  .grid {
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 240px), 1fr));
  }
  .server-card {
    grid-column: 1 / -1;
  }
  .card {
    display: grid;
    align-content: start;
    gap: 10px;
    min-width: 0;
    padding: var(--space-3);
  }
  header {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--space-2);
  }
  .endpoint-failure {
    color: var(--err);
    font-size: var(--type-sm);
    line-height: 1.45;
  }
  .kv {
    --kv-label: 6.5rem;
  }
  .license {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: var(--space-2);
    padding: 0 var(--space-1);
    color: var(--text-soft);
    font-size: var(--type-xs);
  }
  .protocols {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-1);
  }
  .capabilities-card[open] summary {
    margin-bottom: var(--space-1);
  }
  .diagnostics-card > summary {
    padding: var(--space-3);
  }
  .diagnostics-card[open] > summary {
    border-bottom: 1px solid var(--border);
  }
  /* Full-bleed against a clipping card, so the ring goes inside the edge. */
  .diagnostics-card > summary:focus-visible {
    outline-offset: -2px;
  }
  .diagnostics {
    display: grid;
    justify-items: start;
    gap: var(--space-3);
    padding: var(--space-3);
  }
</style>
