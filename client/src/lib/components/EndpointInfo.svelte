<script lang="ts">
  /* ============================================================
   * <EndpointInfo> — Info drawer › Endpoint
   * Read-only card grid: client identity (IP + build version), the
   * wired engine (per-runner version + supported transports per
   * role), the probed server, and the active transport setup.
   * Client/Engine facts are build-static; Server/Transport rows
   * fall back to "—" until the pre-test probe resolves.
   * ============================================================ */
  import { store } from "../state/store.svelte";
  import { pointerIntent } from "../actions/pointerIntent";
  import { fmtMs } from "../format";
  import { BUILD } from "../buildenv";

  const infra = $derived(store.infra);
  const engine = $derived(store.engineInfo);

  const cards = $derived.by(() => {
    const i = infra;
    const e = engine;
    return [
      {
        title: "Client",
        rows: [
          ["IP", i?.clientIp ?? "—"],
          ["Version", BUILD.clientVersion],
        ] as [string, string][],
      },
      {
        // The engine ships with (and is versioned by) the client build today;
        // the per-role transport lists are its capabilities — a future UI lets
        // the user pick latency/throughput transports from exactly these.
        title: "Engine",
        rows: [
          ["Runner", e?.name ?? "—"],
          ["Version", e?.version ?? "—"],
          ["Latency", e ? e.latencyTransports.join(", ") : "—"],
          ["Transfer", e ? e.throughputTransports.join(", ") : "—"],
        ] as [string, string][],
      },
      {
        title: "Server",
        rows: [
          ["Node", i?.server.name ?? store.config.endpoint.host],
          ["Host", i?.server.host ?? store.config.endpoint.host],
          ["Port", String(i?.server.port ?? store.config.endpoint.port)],
          ["Location", i?.server.location ?? "—"],
          ["Version", i?.engineVersion ?? "—"],
          ["Protocol", i?.protocolNegotiated ?? "—"],
        ] as [string, string][],
      },
      {
        title: "Transport",
        rows: [
          ["Transfer", "HTTP streams"],
          ["Latency", "WebSocket"],
          ["Streams", `auto (≤${store.config.parallelStreams})`],
          ["Pre-test ping", i ? `${fmtMs(i.preTestPingMs)} ms` : "—"],
        ] as [string, string][],
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
          {#each c.rows as [dt, dd] (dt)}
            <div>
              <dt>{dt}</dt>
              <dd>{dd}</dd>
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
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
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
    grid-template-columns: minmax(70px, max-content) minmax(0, 1fr);
    gap: 10px;
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

  .hint {
    margin: 0;
    color: var(--text-soft);
    font-family: var(--font-mono);
    font-size: 10px;
  }
</style>
