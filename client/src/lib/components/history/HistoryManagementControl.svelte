<script lang="ts">
  import { ICON } from "../../constants";
  import MoreMenu from "../MoreMenu.svelte";

  let { onClear }: { onClear: (invoker: HTMLElement) => void } = $props();
</script>

<MoreMenu label="Archive management" archive>
  {#snippet children(select)}
    <button
      type="button"
      role="menuitem"
      tabindex="-1"
      onclick={() => select(onClear)}
    >
      <span>{@html ICON.trash}</span>
      <span><strong>Clear all saved results</strong></span>
    </button>
  {/snippet}
</MoreMenu>
