<script lang="ts">
  /* ============================================================
   * <PhaseRail> — five lifecycle segments below the reactor (§3.1)
   * W L D U ✓. Disabled stages render as hollow dashed slots.
   * Active segment fills per phaseFraction; completed → solid ok.
   * ============================================================ */
  import { console as store } from "../state/console.svelte";
  import type { Phase } from "../runner/contract";

  const SEGMENTS = [
    { phase: "warmup", label: "W", stage: null },
    { phase: "latency", label: "L", stage: "latency" },
    { phase: "download", label: "D", stage: "download" },
    { phase: "upload", label: "U", stage: "upload" },
    { phase: "complete", label: "✓", stage: null },
  ] as const;

  const ORDER: Phase[] = ["warmup", "latency", "download", "upload", "complete"];

  type SegState = "disabled" | "completed" | "active" | "pending";

  const segs = $derived.by(() => {
    const cur = store.phase;
    const curI = ORDER.indexOf(cur); // -1 for idle/aborted/error
    return SEGMENTS.map((seg, i) => {
      const enabled = seg.stage ? store.config.stages[seg.stage] : true;
      let state: SegState = "pending";
      let fill = 0;
      if (!enabled) {
        state = "disabled";
      } else if (curI === -1) {
        state = "pending";
      } else if (i < curI) {
        state = "completed";
        fill = 100;
      } else if (i === curI) {
        state = "active";
        fill = store.phaseFraction * 100;
      }
      if (seg.phase === "complete" && cur === "complete") {
        state = "completed";
        fill = 100;
      }
      const fillColor = state === "completed" ? "var(--ok)" : `var(--phase-${seg.phase})`;
      return { phase: seg.phase, label: seg.label, state, fill, fillColor };
    });
  });
</script>

<div class="phase-rail" aria-hidden="true">
  {#each segs as s (s.phase)}
    <div class="seg-col">
      <div class="phase-seg" class:phase-seg--disabled={s.state === "disabled"}>
        {#if s.state === "active" || s.state === "completed"}
          <div class="phase-seg__fill" style="width:{s.fill}%; background:{s.fillColor}"></div>
        {/if}
      </div>
      <span class="seg-label" class:dim={s.state === "disabled"}>{s.label}</span>
    </div>
  {/each}
</div>

<style>
  .phase-rail {
    display: grid;
    grid-template-columns: repeat(5, 1fr);
    gap: 6px;
  }
  .seg-col {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 5px;
  }
  .phase-seg {
    width: 100%;
    height: 6px;
    border-radius: var(--radius-xs);
    background: var(--surface-inset);
    overflow: hidden;
    transition: background var(--dur-slide) var(--ease-snap);
  }
  .phase-seg__fill {
    height: 100%;
    border-radius: inherit;
    transition: width var(--dur-graph) var(--ease-out);
  }
  .phase-seg--disabled {
    background: repeating-linear-gradient(
      45deg,
      var(--surface-inset) 0 4px,
      transparent 4px 8px
    );
  }
  .seg-label {
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--text-soft);
  }
  .seg-label.dim {
    opacity: 0.4;
  }
</style>
