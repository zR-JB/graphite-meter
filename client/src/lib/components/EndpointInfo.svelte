<script lang="ts">
  import { announce } from "../presentation/announcer.svelte";
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
  import {
    MISSING,
    reasonLabel,
    transportLabel,
  } from "../presentation/vocabulary";
  import Disclosure from "./Disclosure.svelte";
  import ServerScope from "./ServerScope.svelte";

  type PathRole = "throughput" | "latency";
  const PATH_ROLES = ["throughput", "latency"] as const;
  let { onOpenLegal }: { onOpenLegal: () => void } = $props();
  let inspectedServer = $state("");
  const availableServers = $derived(
    store.run?.servers.map((entry) => entry.server) ??
      catalogSelection(store.serverCatalog, store.selectedServers),
  );
  const selectedServer = $derived(
    availableServers.find((server) => server.id === inspectedServer) ??
      availableServers.find((server) => server.id === store.latencyFocus) ??
      availableServers[0],
  );
  // Inspection never changes selection; completed runs keep their prepared paths.
  const activePaths = $derived(
    store.run?.servers.find((entry) => entry.server.id === selectedServer?.id)
      ?.paths ?? null,
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
    return `${carrier} over ${connections.throughput.carrier}`;
  });
  const serverLoad = $derived(
    serverLoadSummary(
      (activePaths?.throughput ?? validation.throughput.path)?.probe.load,
    ),
  );
  const httpPaths = $derived(advertisedServerHttpPaths(discovery));

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

  let copiedTimer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => () => clearTimeout(copiedTimer));
  async function copyReport() {
    clearTimeout(copiedTimer);
    try {
      await navigator.clipboard.writeText(diagnosticReport());
      copied = true;
      // Not motion: the copy confirmation lingers briefly.
      copiedTimer = setTimeout(() => (copied = false), 1500);
      announce("Diagnostic report copied");
    } catch {
      copied = false;
      announce("Unable to copy diagnostic report");
    }
  }
</script>

<section class="infra">
  {#if availableServers.length > 1}
    <ServerScope
      servers={availableServers}
      value={selectedServer?.id ?? ""}
      label="Inspect server"
      onchange={(id) => (inspectedServer = id)}
    />
  {/if}
  {#each failures as failure}
    <p class="notice" data-tone="err">{reasonLabel(failure.reason)}</p>
  {/each}
  <Disclosure
    class="surface-inset server-card"
    title={pathMode === "live" ? "Selected server" : "Tested server"}
    facts={[server?.name ?? "Checking server", server?.location]
      .filter(Boolean)
      .join(" · ")}
  >
    <dl class="kv">
      {#if selectedServer}
        <div>
          <dt>Address</dt>
          <dd>{selectedServer.url}</dd>
        </div>
      {/if}
      <div>
        <dt>Location</dt>
        <dd>{server?.location ?? "Unavailable"}</dd>
      </div>
    </dl>
    <p class="hint">
      {pathMode === "live"
        ? "Current selection and verified connections."
        : pathMode === "running"
          ? "Connections used by this test."
          : "Connections captured for the displayed result."}
    </p>
  </Disclosure>

  {#each PATH_ROLES as role}
    {@const connection = connections[role]}
    {@const status = endpointPathStatus(connection.validation, pathMode)}
    {@const inTest = role === "throughput" || latencyRequested}
    <Disclosure
      class="surface-inset path"
      title={`${role} path`}
      facts={inTest
        ? connection.summary
        : "Not selected for latency measurement"}
    >
      {#snippet aside()}
        <span class="badge" data-tone={inTest ? status.tone : "neutral"}
          >{inTest
            ? status.label
            : pathMode === "live"
              ? "Not selected"
              : "Not in test"}</span
        >
      {/snippet}
      <dl class="kv">
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
    </Disclosure>
  {/each}

  <Disclosure
    class="surface-inset"
    title="Server capabilities"
    facts={capabilities("throughput")}
  >
    <dl class="kv">
      <div>
        <dt>HTTP versions</dt>
        <dd>
          {httpPaths === null
            ? "Checking server"
            : httpPaths.join(", ") || "None advertised"}
        </dd>
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
  </Disclosure>

  <Disclosure
    class="surface-inset"
    title="Diagnostics"
    facts={`Client ${BUILD.identity} · server ${discovery?.engineVersion ?? MISSING}`}
  >
    <dl class="kv">
      <div>
        <dt>Server instance</dt>
        <dd>{discovery?.generation || MISSING}</dd>
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
  </Disclosure>
  <p class="license">
    <span>Legal</span>
    <button class="btn btn-quiet" type="button" onclick={onOpenLegal}
      >About &amp; legal</button
    >
  </p>
</section>

<style>
  .infra {
    display: grid;
    gap: var(--space-2);
  }
  .kv {
    --kv-label: 6.5rem;
  }
  .btn {
    justify-self: start;
  }
  .license {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: var(--space-2);
    padding: 0 var(--space-1);
    color: var(--text-soft);
    font-size: var(--type-xs);
  }
</style>
