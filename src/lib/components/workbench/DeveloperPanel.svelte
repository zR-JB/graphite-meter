<script lang="ts">
  /* ============================================================
   * <DeveloperPanel> — Workbench › Developer (§13.6)
   * Dev-only affordance: injects LIVE anomalies into the running
   * engine via wire.injectAnomaly. Each fires relative to the
   * current moment in the active phase, so the buttons are only
   * enabled while a run is in flight (`running`).
   * ============================================================ */
  import { injectAnomaly } from "../../runner/wire.svelte";
  import { pointerIntent } from "../../actions/pointerIntent";
  import type { RunnerAnomaly } from "../../runner/contract";

  interface Props {
    running?: boolean;
  }
  let { running = false }: Props = $props();

  const CARDS: {
    kind: RunnerAnomaly["kind"];
    title: string;
    desc: string;
    cta: string;
  }[] = [
    {
      kind: "latency-spike",
      title: "Latency Spike",
      desc: "Trip a temporary RTT spike — watch the connectivity pulse and latency profile react.",
      cta: "Inject Spike",
    },
    {
      kind: "packet-loss",
      title: "Packet Loss",
      desc: "Raise loss probability briefly — clay-red loss cuts appear in the telemetry.",
      cta: "Inject Loss",
    },
    {
      kind: "throughput-drop",
      title: "Throughput Drop",
      desc: "Cut receive/send throughput for a window — stress the gauge and chart stability.",
      cta: "Inject Drop",
    },
  ];

  function fire(kind: RunnerAnomaly["kind"]) {
    injectAnomaly({ kind } as RunnerAnomaly);
  }
</script>

<section class="dev">
  <div class="head">
    <h3>Developer Simulation</h3>
    <p>Dev-only anomaly injection. Live perturbations fire into the active run.</p>
    <span class="badge">DEV ONLY</span>
  </div>

  {#if !running}
    <p class="hint">Start a run to enable injection — anomalies fire relative to the current phase.</p>
  {/if}

  <div class="grid">
    {#each CARDS as c (c.kind)}
      <article class="card" use:pointerIntent={{ disabled: !running }}>
        <h4>{c.title}</h4>
        <p>{c.desc}</p>
        <button
          disabled={!running}
          title="{c.title}: inject this anomaly live into the running test"
          onclick={() => fire(c.kind)}>{c.cta}</button
        >
      </article>
    {/each}
  </div>
</section>

<style>
  .dev {
    display: grid;
    gap: 14px;
  }
  .head {
    display: grid;
    gap: 3px;
    position: relative;
  }
  .head h3 {
    margin: 0;
    color: var(--text);
    font-size: 16px;
    font-weight: 850;
    letter-spacing: -0.03em;
  }
  .head p {
    margin: 0;
    color: var(--text-soft);
    font-family: var(--font-mono);
    font-size: 10px;
  }
  .badge {
    position: absolute;
    top: 0;
    right: 0;
    padding: 2px 8px;
    border: 1px solid color-mix(in srgb, var(--err) 40%, var(--border));
    border-radius: var(--radius-xs);
    background: var(--err-soft);
    color: var(--err);
    font-family: var(--font-mono);
    font-size: 9px;
    font-weight: 800;
    letter-spacing: 0.1em;
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
    gap: 12px;
  }
  .card {
    position: relative;
    display: grid;
    grid-template-rows: auto minmax(0, 1fr) auto;
    align-content: start;
    gap: 10px;
    min-width: 0;
    min-height: 150px;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: linear-gradient(180deg, var(--surface-2), transparent),
      var(--surface-inset);
    padding: 14px;
    overflow: clip;
    transition:
      transform var(--dur-hover) var(--ease-out),
      border-color var(--dur-hover) var(--ease-out);
  }
  .card::before {
    content: "";
    position: absolute;
    inset: 0;
    pointer-events: none;
    opacity: 0;
    background: radial-gradient(
      180px circle at var(--intent-x, 50%) var(--intent-y, 0),
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
    color: var(--text);
    font-size: 12px;
    font-weight: 840;
  }
  .card p {
    margin: 0;
    color: var(--text-soft);
    font-family: var(--font-mono);
    font-size: 10px;
    line-height: 1.55;
  }

  button {
    width: 100%;
    min-height: 36px;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface-2);
    color: var(--text);
    padding: 8px 10px;
    font-size: 12px;
    font-weight: 800;
    cursor: pointer;
    transition:
      transform var(--dur-hover) var(--ease-out),
      border-color var(--dur-hover) var(--ease-out),
      background var(--dur-hover) var(--ease-out),
      color var(--dur-hover) var(--ease-out);
  }
  button:hover:not(:disabled) {
    transform: translateY(-1px);
    border-color: color-mix(in srgb, var(--brand) 32%, var(--border-strong));
    background: var(--brand-soft);
    color: var(--brand-strong);
  }
  button:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }

  .hint {
    margin: 0;
    color: var(--text-soft);
    font-family: var(--font-mono);
    font-size: 10px;
  }
</style>
