<script lang="ts">
  // SettingsPanel gates this tab on GM_CLIENT_DEV_TOOLS.
  // GM_CLIENT_ALLOW_DUMMY gates the simulation separately. A dev-tools build
  // without the dummy runner keeps debug logging and drops anomaly injection.
  import { store } from "../../state/store.svelte";
  import Switch from "../Switch.svelte";
  import DeveloperSimulation from "./DeveloperSimulation.svelte";

  interface Props {
    running?: boolean;
  }
  let { running = false }: Props = $props();
</script>

<section class="dev">
  <div class="diag">
    <div class="diag-text">
      <h4>Debug logging</h4>
      <p>
        Verbose, component-tagged console diagnostics — per-stream raw
        throughput (<code>dl-worker#n</code> / <code>ul-worker#n</code>), the
        aggregated pool rate, and the de-aliasing EMA. Pair with the server's
        <code>-verbose</code> flag to compare client- and server-side rates against
        the kernel interface.
      </p>
    </div>
    <Switch bind:checked={store.debugLogging} label="Console" />
  </div>

  {#if __GM_ALLOW_DUMMY__}
    <DeveloperSimulation {running} />
  {/if}
</section>

<style>
  .dev {
    display: grid;
    gap: 14px;
  }
  .diag {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-4);
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-inset);
    box-shadow: var(--elev-recess);
    padding: var(--space-3);
  }
  .diag-text {
    display: grid;
    gap: var(--space-1);
    min-width: 0;
  }
  .diag-text h4 {
    margin: 0;
    color: var(--text);
    font-size: 13px;
    font-weight: 840;
  }
  .diag-text p {
    margin: 0;
    color: var(--text-soft);
    font-family: var(--font-mono);
    font-size: 10px;
    line-height: 1.55;
  }
  .diag-text code {
    font-family: var(--font-mono);
    color: var(--text);
  }
</style>
