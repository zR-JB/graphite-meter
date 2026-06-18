<script lang="ts">
  /* ============================================================
   * <InfraCard> — right inspector, top (§3.7)
   * Definition list from console.infra (populated by runner.probe
   * on mount). Skeleton shimmer rows while probing.
   * ============================================================ */
  import { console as store } from "../state/console.svelte";
  import { fmtMs } from "../format";
  import { ICON } from "../constants";

  const rows = $derived.by(() => {
    const i = store.infra;
    if (!i) return null;
    return [
      { label: "Client IP", value: i.clientIp },
      { label: "Server", value: `${i.server.name}${i.server.location ? ` · ${i.server.location}` : ""}` },
      { label: "Port", value: String(i.server.port) },
      { label: "Pre-test ping", value: `${fmtMs(i.preTestPingMs)} ms` },
      { label: "Engine", value: i.engineVersion },
      { label: "Protocol", value: i.protocolNegotiated },
    ];
  });
</script>

<section class="card">
  <header class="card-head">
    <span class="head-ico">{@html ICON.server}</span>
    <h3>Infrastructure</h3>
  </header>

  <dl class="body">
    {#if rows}
      {#each rows as r (r.label)}
        <div class="row">
          <dt>{r.label}</dt>
          <dd>{r.value}</dd>
        </div>
      {/each}
    {:else}
      {#each Array(6) as _, i (i)}
        <div class="row">
          <dt><span class="skel skel-label"></span></dt>
          <dd><span class="skel skel-value"></span></dd>
        </div>
      {/each}
    {/if}
  </dl>
</section>

<style>
  .card {
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-1);
    box-shadow: var(--elev-tile);
    overflow: clip;
  }
  .card-head {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-3);
    border-bottom: 1px solid var(--border);
    background: linear-gradient(180deg, var(--surface-2), transparent);
  }
  .head-ico {
    display: grid;
    place-items: center;
    width: 34px;
    height: 34px;
    border-radius: var(--r-well);
    border: 1px solid color-mix(in srgb, var(--signal) 34%, var(--border));
    background: var(--signal-soft);
    color: var(--signal);
  }
  .head-ico :global(svg) {
    width: 18px;
    height: 18px;
  }
  .card-head h3 {
    font-size: 13px;
    font-weight: 820;
    letter-spacing: -0.02em;
    margin: 0;
  }

  .body {
    margin: 0;
    padding: 6px 14px 12px;
  }
  .row {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
    padding: 8px 0;
    border-bottom: 1px solid var(--border-subtle);
  }
  .row:last-child {
    border-bottom: 0;
  }
  dt {
    flex: none;
    font-size: 11px;
    color: var(--text-soft);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  dd {
    margin: 0;
    text-align: right;
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--text);
    overflow-wrap: anywhere;
  }

  /* Skeleton shimmer */
  .skel {
    display: inline-block;
    height: 11px;
    border-radius: var(--radius-xs);
    background: linear-gradient(
      90deg,
      var(--surface-2) 0%,
      var(--surface-3) 50%,
      var(--surface-2) 100%
    );
    background-size: 200% 100%;
    animation: shimmer 1.2s linear infinite;
  }
  .skel-label {
    width: 64px;
  }
  .skel-value {
    width: 96px;
  }
  @keyframes shimmer {
    from {
      background-position: 200% 0;
    }
    to {
      background-position: -200% 0;
    }
  }
</style>
