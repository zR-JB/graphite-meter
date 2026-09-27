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
  import { formatLatency } from "../format";
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
    JARGON,
    MISSING,
    reasonLabel,
    transportLabel,
  } from "../presentation/vocabulary";
  import { tipGroup, tooltip } from "../actions/tooltip";
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

{#snippet row(label: string, value: string, tip?: string)}
  <div {@attach tip ? tooltip(() => tip) : null}>
    <dt>{label}</dt>
    <dd>{value}</dd>
  </div>
{/snippet}

<section class="infra">
  <div class="group server-card">
    <div class="group-head">
      <h3 class="caps">
        {pathMode === "live" ? "Selected server" : "Tested server"}
      </h3>
      {#if availableServers.length > 1}
        <ServerScope
          servers={availableServers}
          value={selectedServer?.id ?? ""}
          label="Inspect server"
          onchange={(id) => (inspectedServer = id)}
        />
      {/if}
    </div>
    {#each failures as failure}
      <p class="notice" data-tone="err">{reasonLabel(failure.reason)}</p>
    {/each}
    <dl class="kv" data-tip-group {@attach tipGroup}>
      {@render row("Name", server?.name ?? "Checking server")}
      {#if selectedServer}{@render row("Address", selectedServer.url)}{/if}
      {#if server?.location}{@render row("Location", server.location)}{/if}
      {#if serverLoad}{@render row("Load", serverLoad, JARGON.serverLoad)}{/if}
    </dl>
  </div>

  <div class="group">
    <h3 class="caps">Connection</h3>
    <dl class="kv" data-tip-group {@attach tipGroup}>
      {#each PATH_ROLES as role}
        {@const connection = connections[role]}
        {@const status = endpointPathStatus(connection.validation, pathMode)}
        {@const inTest = role === "throughput" || latencyRequested}
        <div
          class="path"
          data-role={role}
          {@attach tooltip(() => JARGON[`${role}Path`])}
        >
          <dt>{role === "throughput" ? "Throughput path" : "Latency path"}</dt>
          <dd>
            {inTest ? connection.summary : "Not selected"}
            <span class="badge" data-tone={inTest ? status.tone : "neutral"}
              >{inTest
                ? status.label
                : pathMode === "live"
                  ? "Not selected"
                  : "Not in test"}</span
            >
          </dd>
        </div>
      {/each}
      {@render row(
        "Evidence",
        pathEvidence(
          "throughput",
          connections.throughput.browserProtocol,
          connections.throughput.serverProtocol,
        ),
        JARGON.pathEvidence,
      )}
      {@render row("Streams", transferStreams, JARGON.forcedStreams)}
      {@render row("Upload feed", uploadProgressPath, JARGON.uploadFeed)}
      {#if latencyRequested}
        {@render row(
          "Pre-test latency",
          connections.latency.preTestPingMs !== undefined
            ? formatLatency(connections.latency.preTestPingMs)
            : "Pending",
          JARGON.pretestLatency,
        )}
      {/if}
      {@render row(
        "Your address",
        clientEvidence("throughput"),
        JARGON.clientAddress,
      )}
    </dl>
  </div>

  <div class="group">
    <h3 class="caps">Server supports</h3>
    <dl class="kv" data-tip-group {@attach tipGroup}>
      {@render row(
        "HTTP",
        httpPaths === null
          ? "Checking server"
          : httpPaths.join(", ") || "None advertised",
      )}
      {@render row("Throughput", capabilities("throughput"))}
      {@render row("Latency", capabilities("latency"))}
    </dl>
  </div>

  <div class="group">
    <h3 class="caps">Build</h3>
    <dl class="kv" data-tip-group {@attach tipGroup}>
      {@render row(
        "Client",
        [BUILD.profile, BUILD.version && `v${BUILD.version}`, BUILD.revision]
          .filter(Boolean)
          .join(" · "),
      )}
      {@render row(
        "Server",
        discovery?.engineVersion ?? MISSING,
        discovery?.generation
          ? `${JARGON.serverInstance}\nInstance ${discovery.generation}`
          : undefined,
      )}
    </dl>
  </div>

  <p class="actions">
    <button class="btn copy" type="button" onclick={copyReport}>
      <span class:hidden={copied}>Copy diagnostic report</span>
      <span class:hidden={!copied} aria-hidden={!copied}>Copied</span>
    </button>
    <button class="btn btn-quiet" type="button" onclick={onOpenLegal}
      >About &amp; legal</button
    >
  </p>
</section>

<style>
  /* Both labels share one cell, so the button keeps its width. */
  .copy {
    display: inline-grid;
  }
  .copy > span {
    grid-area: 1 / 1;
  }
  .copy > .hidden {
    visibility: hidden;
  }
  .infra {
    display: grid;
    gap: var(--space-4);
  }
  .group-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
  }
  .path dd {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    align-items: start;
    gap: var(--space-2);
  }
  .actions {
    display: flex;
    justify-content: space-between;
    gap: var(--space-2);
  }
</style>
