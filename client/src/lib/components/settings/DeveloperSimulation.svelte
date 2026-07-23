<script lang="ts">
  // Dev-only anomaly controls, tree-shaken from real-only builds.
  import { injectAnomaly } from "../../runner/engine.svelte";
  import { tooltip } from "../../actions/tooltip";
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
    {
      kind: "connection-drop",
      title: "Connection Drop",
      desc: "Kill the link for ~4s — the gauge grinds to 0, the run pauses, then resumes and finishes late by the dropped time.",
      cta: "Drop Connection",
    },
  ];

  function fire(kind: RunnerAnomaly["kind"]) {
    injectAnomaly({ kind } as RunnerAnomaly);
  }
</script>

<div class="head">
  <h3>Developer Simulation</h3>
  <p>
    Dev-only anomaly injection. Live perturbations fire into the active run.
  </p>
  <span class="badge">DEV ONLY</span>
</div>

{#if !running}
  <p class="hint">
    Start a run to enable injection — anomalies fire relative to the current
    phase.
  </p>
{/if}

<div class="grid">
  {#each CARDS as c (c.kind)}
    <article class="card">
      <h4>{c.title}</h4>
      <p>{c.desc}</p>
      <button
        disabled={!running}
        use:tooltip={`${c.title}: inject this anomaly live into the running test`}
        onclick={() => fire(c.kind)}>{c.cta}</button
      >
    </article>
  {/each}
</div>

<style>
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
    border-radius: var(--r-well);
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
    gap: var(--space-3);
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
  .card:hover {
    transform: translateY(-1px);
    border-color: var(--border-strong);
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
    border-radius: var(--r-chrome);
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
  button:focus-visible {
    outline: var(--focus-ring);
    outline-offset: 2px;
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
