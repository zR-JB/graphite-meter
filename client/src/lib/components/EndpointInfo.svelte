<script lang="ts">
  // Responsive endpoint facts for the info drawer. Probe-dependent values
  // remain visibly pending until the handshake resolves.
  import { store } from "../state/store.svelte";
  import { pointerIntent } from "../actions/pointerIntent";
  import { fmtMs } from "../format";
  import { BUILD } from "../buildenv";
  import { describeTransferStreams } from "../runner/real/streamPolicy";

  const infra = $derived(store.infra);
  const engine = $derived(store.engineInfo);

  type Fact = { label: string; value: string | string[] };
  type Card = { title: string; rows: Fact[] };

  function endpointAddress(host: string, port: number): string {
    const address =
      host.includes(":") && !host.startsWith("[") ? `[${host}]` : host;
    return `${address}:${port}`;
  }

  const cards = $derived.by<Card[]>(() => {
    const i = infra;
    const e = engine;
    return [
      {
        title: "Client",
        rows: [
          { label: "Address", value: i?.clientIp ?? "—" },
          {
            label: "Observed via",
            value: i
              ? `IPv${i.clientIpVersion} · ${
                  i.clientIpSource === "forwarded"
                    ? "Trusted proxy"
                    : "Socket peer"
                }`
              : "—",
          },
          { label: "App version", value: BUILD.clientVersion },
        ],
      },
      {
        title: "Engine",
        rows: [
          { label: "Runner", value: e?.name ?? "—" },
          { label: "Version", value: e?.version ?? "—" },
          { label: "Latency", value: e?.latencyTransports ?? "—" },
          { label: "Transfer", value: e?.throughputTransports ?? "—" },
        ],
      },
      {
        title: "Server",
        rows: [
          {
            label: "Node",
            value: i?.server.name ?? store.config.endpoint.host,
          },
          { label: "Location", value: i?.server.location ?? "—" },
          {
            label: "Endpoint",
            value: endpointAddress(
              i?.server.host ?? store.config.endpoint.host,
              i?.server.port ?? store.config.endpoint.port,
            ),
          },
          { label: "Version", value: i?.engineVersion ?? "—" },
        ],
      },
      {
        title: "Connection",
        rows: [
          {
            label: "Selected target",
            value: i?.selectedTarget ?? "Current origin",
          },
          {
            label: "Verified browser",
            value: i?.firstHopProtocol ?? "Scheme fallback",
          },
          { label: "Server observed", value: i?.protocolNegotiated ?? "—" },
          { label: "Transfer", value: "Fetch streams" },
          { label: "Latency / progress", value: "WebSocket over TCP" },
          {
            label: "Streams",
            value: describeTransferStreams(
              store.config.transferStreams,
              i?.selectedTarget,
            ),
          },
          {
            label: "Pre-test ping",
            value: i ? `${fmtMs(i.preTestPingMs)} ms` : "—",
          },
        ],
      },
    ];
  });
</script>

<section class="infra">
  <div class="grid">
    {#each cards as c (c.title)}
      <article class="card" use:pointerIntent>
        <h4>{c.title}</h4>
        <dl>
          {#each c.rows as row (row.label)}
            <div>
              <dt>{row.label}</dt>
              <dd>
                {#if Array.isArray(row.value)}
                  <span class="values">
                    {#each row.value as value (value)}
                      <span>{value}</span>
                    {/each}
                  </span>
                {:else}
                  {row.value}
                {/if}
              </dd>
            </div>
          {/each}
        </dl>
      </article>
    {/each}
  </div>

  {#if !infra}
    <p class="hint">
      Probe pending — values populate once the handshake resolves.
    </p>
  {/if}
</section>

<style>
  .infra {
    display: grid;
    gap: 14px;
    container-type: inline-size;
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 260px), 1fr));
    gap: 12px;
  }

  .card {
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
    box-shadow: var(--elev-recess);
    padding: var(--space-3);
    overflow: clip;
    transition:
      transform var(--dur-hover) var(--ease-out),
      border-color var(--dur-hover) var(--ease-out);
  }
  /* Radial hover driven by pointerIntent's --intent-x / --intent-y. */
  .card::before {
    content: "";
    position: absolute;
    inset: 0;
    pointer-events: none;
    opacity: 0;
    background: radial-gradient(
      200px circle at var(--intent-x, 50%) var(--intent-y, 0),
      var(--brand-soft),
      transparent 70%
    );
    transition: opacity var(--dur-hover) var(--ease-out);
  }
  .card:hover {
    transform: translateY(-1px);
    border-color: var(--border-strong);
  }
  .card:hover::before {
    opacity: 1;
  }
  .card > * {
    position: relative;
    z-index: 1;
  }

  h4 {
    margin: 0;
    color: var(--text-soft);
    font-size: 10px;
    font-weight: 850;
    letter-spacing: 0.13em;
    text-transform: uppercase;
  }
  dl {
    display: grid;
    gap: 8px;
    margin: 0;
  }
  dl div {
    display: grid;
    grid-template-columns: minmax(88px, max-content) minmax(0, 1fr);
    gap: 12px;
    align-items: baseline;
  }
  dt {
    color: var(--text-soft);
    font-size: 11px;
    font-weight: 700;
  }
  dd {
    margin: 0;
    min-width: 0;
    color: var(--text);
    font-family: var(--font-mono);
    font-size: 11px;
    overflow-wrap: anywhere; /* transport lists / long hosts wrap, not clip */
  }

  .values {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 6px;
  }
  .values span {
    padding: 2px 5px;
    border-radius: var(--radius-xs);
    background: var(--surface-1);
    white-space: nowrap;
  }

  @container (max-width: 300px) {
    dl div {
      grid-template-columns: 1fr;
      gap: 3px;
    }
  }

  .hint {
    margin: 0;
    color: var(--text-soft);
    font-family: var(--font-mono);
    font-size: 10px;
  }
</style>
