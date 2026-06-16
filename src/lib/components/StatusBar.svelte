<script lang="ts">
  /* ============================================================
   * <StatusBar> — bottom status zone (§1.2)
   * State-machine label, elapsed, bytes transferred, build hash.
   * Owns a local ticker so elapsed advances independently of
   * sample ingest (store.elapsedMs only recomputes on startEpoch).
   * ============================================================ */
  import { onMount } from "svelte";
  import { console as store } from "../state/console.svelte";
  import { fmtBytes } from "../format";
  import { BUILD_HASH } from "../constants";
  import type { Phase } from "../runner/contract";

  const PHASE_LABEL: Record<Phase, string> = {
    idle: "Idle",
    warmup: "Warming up",
    latency: "Measuring latency",
    download: "Downloading",
    upload: "Uploading",
    complete: "Complete",
    aborted: "Aborted",
    error: "Error",
  };

  let now = $state(Date.now());
  onMount(() => {
    const id = setInterval(() => (now = Date.now()), 200);
    return () => clearInterval(id);
  });

  const elapsedMs = $derived(
    store.isRunning && store.startEpoch
      ? now - store.startEpoch
      : (store.result?.durationMs ?? 0),
  );

  function fmtElapsed(ms: number): string {
    const s = ms / 1000;
    return `${s.toFixed(1)}s`;
  }
</script>

<span class="label">{PHASE_LABEL[store.phase]}</span>
<span class="sep">·</span>
<span>elapsed {fmtElapsed(elapsedMs)}</span>
<span class="sep">·</span>
<span>{fmtBytes(store.bytesTransferred, store.unitBase)} xfer</span>
<span class="flex-1"></span>
<span class="soft">build {BUILD_HASH}</span>

<style>
  .label {
    color: var(--text);
    font-weight: 600;
  }
  .sep {
    color: var(--text-soft);
  }
  .flex-1 {
    flex: 1;
  }
  .soft {
    color: var(--text-soft);
  }
</style>
