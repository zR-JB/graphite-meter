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
  import { getApplicationController } from "../runner/controllerContext";
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
  import { JARGON, MISSING, transportLabel } from "../presentation/vocabulary";
  import { serverIssues } from "../presentation/resultSummary";
  import { term, tipGroup, tooltip } from "../actions/tooltip";
  import ServerScope from "./ServerScope.svelte";
  import Icon from "./Icon.svelte";

  type PathRole = "throughput" | "latency";
  const PATH_ROLES = ["throughput", "latency"] as const;
  let { onOpenLegal }: { onOpenLegal: () => void } = $props();
  const controller = getApplicationController();
  const availableServers = $derived(
    store.run?.servers.map((entry) => entry.server) ??
      catalogSelection(store.serverCatalog, store.selectedServers),
  );
  // Details follows the lens: its server, else the one whose latency is shown.
  const selectedServer = $derived(
    availableServers.find((server) => server.id === store.resultScope) ??
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
      : latencyPathNeeded(store.config),
  );
  const failures = $derived(
    store.serverDetails?.failures.filter(
      (failure) => failure.serverId === selectedServer?.id,
    ) ?? [],
  );
  // One note per reason, naming the stages it ended, as the results row does.
  const issues = $derived(
    store.serverDetails && selectedServer
      ? serverIssues(store.serverDetails, selectedServer.id)
      : [],
  );
  let copied = $state(false);

  const pathMode = $derived(
    store.isRunning ? "running" : activePaths ? "result" : "live",
  );

  const client = $derived.by(() => {
    const { clientIp, clientIpVersion, clientIpSource } =
      connections.throughput;
    if (!clientIp) return { value: pending };
    return {
      value: clientIp,
      aside: `IPv${clientIpVersion}, ${clientIpSource === "forwarded" ? "trusted proxy" : "socket peer"}`,
    };
  });

  function capabilities(role: PathRole): string[] | string {
    const advertised = advertisedServerCapabilities(discovery, role);
    if (!advertised) return "Checking server";
    const values = advertised.transports.map((value) =>
      transportLabel(value, role),
    );
    if (!values.length) return "None advertised";
    return advertised.browserBlocked
      ? [...values, "Some clear origins blocked by this page"]
      : values;
  }
  const unreachable = $derived(
    !failures.length && selectedServer
      ? store.servers.get(selectedServer.id)?.readiness === "failed"
      : false,
  );
  // A failed server's missing facts will not arrive.
  const pending = $derived(unreachable ? MISSING : "Pending");

  // The feed rides the session carrying the bytes, or its own fetch.
  const uploadProgressPath = $derived.by(() => {
    const target = connections.throughput.target;
    if (!target) return pending;
    const carrier =
      target.transport === "fetch-stream" ? "Fetch stream" : "Session stream";
    return `${carrier} over ${connections.throughput.carrier}`;
  });
  // A failed server's last load reading no longer describes it.
  const serverLoad = $derived(
    failures.length ||
      (selectedServer &&
        store.servers.get(selectedServer.id)?.readiness === "failed")
      ? null
      : serverLoadSummary(
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

{#snippet row(
  label: string,
  fact: string | { value: string; aside?: string },
  tip?: string,
  marked = false,
)}
  {@const { value, aside } = typeof fact === "string" ? { value: fact } : fact}
  <div>
    <dt {@attach tip ? (marked ? term : tooltip)(() => tip) : null}>{label}</dt>
    <dd>
      {value}{#if aside}<span class="aside">{aside}</span>{/if}
    </dd>
  </div>
{/snippet}

{#snippet list(label: string, items: string[] | string)}
  <div>
    <dt>{label}</dt>
    <dd class="list">
      {#each typeof items === "string" ? [items] : items as item (item)}<span
          >{item}</span
        >{/each}
    </dd>
  </div>
{/snippet}

<section class="infra">
  <div class="group server-card">
    <h3>{pathMode === "live" ? "Selected server" : "Tested server"}</h3>
    <dl class="kv" data-tip-group {@attach tipGroup}>
      {#if availableServers.length > 1}
        <div>
          <dt>Server</dt>
          <dd>
            <ServerScope
              servers={availableServers}
              value={selectedServer?.id ?? ""}
              label="Inspect server"
              onchange={controller.showServer}
              disabled={store.isRunning}
            />
          </dd>
        </div>
      {/if}
      {@render row("Name", server?.name ?? "Checking server")}
      {#if selectedServer}{@render row("Address", selectedServer.url)}{/if}
      {#if server?.location}{@render row("Location", server.location)}{/if}
      {#if serverLoad}{@render row(
          "Load",
          serverLoad,
          JARGON.serverLoad,
          true,
        )}{/if}
    </dl>
    {#each issues as issue}
      <p class="notice" data-tone="err">
        <strong>{issue.stages}</strong>
        {issue.reason}
      </p>
    {:else}
      {#if unreachable}<p class="notice" data-tone="err">
          {validation.throughput.message ?? "Server could not be reached"}
        </p>{/if}
    {/each}
  </div>

  <div class="group">
    <h3>Connection</h3>
    <dl class="kv" data-tip-group {@attach tipGroup}>
      {#each PATH_ROLES as role}
        {@const connection = connections[role]}
        {@const status = endpointPathStatus(connection.validation, pathMode)}
        {@const inTest = role === "throughput" || latencyRequested}
        <div class="path" data-role={role}>
          <dt {@attach tooltip(() => JARGON[`${role}Path`])}>
            {role === "throughput" ? "Throughput path" : "Latency path"}
          </dt>
          <dd>
            <span
              >{inTest
                ? connection.summary
                : pathMode === "live"
                  ? "Not selected"
                  : "Not measured"}</span
            >
            {#if inTest}<span class="path-status"
                >{#if status.tone !== "neutral"}<span
                    class="status-dot inline"
                    data-tone={status.tone}
                    aria-hidden="true"
                  ></span>{/if}{status.label}</span
              >{/if}
          </dd>
        </div>
      {/each}
      {@render row(
        "Evidence",
        pathEvidence(
          "throughput",
          connections.throughput.browserProtocol,
          connections.throughput.serverProtocol,
          pending,
        ),
        JARGON.pathEvidence,
        true,
      )}
      {@render row("Streams", transferStreams, JARGON.forcedStreams)}
      {@render row("Upload feed", uploadProgressPath, JARGON.uploadFeed)}
      {#if latencyRequested}
        {@render row(
          "Pre-test latency",
          connections.latency.preTestPingMs !== undefined
            ? formatLatency(connections.latency.preTestPingMs)
            : pending,
          JARGON.pretestLatency,
        )}
      {/if}
      {@render row("Your address", client, JARGON.clientAddress)}
    </dl>
  </div>

  <div class="group">
    <h3>Server supports</h3>
    <dl class="kv" data-tip-group {@attach tipGroup}>
      {@render list(
        "HTTP",
        httpPaths === null
          ? "Checking server"
          : httpPaths.length
            ? httpPaths
            : "None advertised",
      )}
      {@render list("Throughput", capabilities("throughput"))}
      {@render list("Latency", capabilities("latency"))}
    </dl>
  </div>

  <div class="group">
    <h3>Build</h3>
    <dl class="kv" data-tip-group {@attach tipGroup}>
      {@render row("Client", {
        value: BUILD.version ? `v${BUILD.version}` : BUILD.revision,
        aside: [BUILD.profile, BUILD.version && BUILD.revision]
          .filter(Boolean)
          .join(", "),
      })}
      {@render row(
        "Server",
        discovery?.engineVersion ?? MISSING,
        discovery?.generation
          ? `${JARGON.serverInstance}\nInstance ${discovery.generation}`
          : undefined,
      )}
    </dl>
  </div>

  <div class="kv">
    <button class="link-row copy" type="button" onclick={copyReport}>
      <span class:hidden={copied}>Copy diagnostic report</span>
      <span class:hidden={!copied} aria-hidden={!copied}>Copied</span>
      <Icon name={copied ? "check" : "copy"} />
    </button>
    <button class="link-row" type="button" onclick={onOpenLegal}
      >About &amp; legal<Icon name="chevron" /></button
    >
  </div>
</section>

<style>
  .infra {
    display: grid;
    gap: var(--space-5);
  }
  .path dd {
    display: grid;
    gap: 2px;
  }
  .path-status {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: var(--text-soft);
    font: var(--role-caption);
  }
  .list {
    display: grid;
  }
  .kv button.link-row {
    min-height: var(--row-h);
    text-align: start;
  }
  /* Both labels share one cell, so the row never reflows. */
  .copy {
    display: grid;
    grid-template-columns: 1fr auto;
  }
  .copy > span {
    grid-area: 1 / 1;
  }
  .copy > .hidden {
    visibility: hidden;
  }
</style>
