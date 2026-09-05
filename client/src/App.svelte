<script lang="ts">
  import { onMount } from "svelte";
  import { mountHistoryPersistence } from "./lib/history/persistence.svelte";
  import Console from "./lib/components/Console.svelte";
  import { store, mountStoreEffects } from "./lib/state/store.svelte";
  import { createApplicationController } from "./lib/runner/engine.svelte";
  import { setApplicationController } from "./lib/runner/controllerContext";

  const controller = setApplicationController(
    createApplicationController(store),
  );
  onMount(() => {
    const disposeStore = mountStoreEffects(store);
    const disposeHistory = mountHistoryPersistence(store);
    void controller.boot();
    return () => {
      controller.dispose();
      disposeHistory();
      disposeStore();
    };
  });
</script>

<Console />
