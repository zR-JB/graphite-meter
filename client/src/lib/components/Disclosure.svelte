<script lang="ts">
  import type { Snippet } from "svelte";

  interface Props {
    title?: string;
    facts?: string;
    aside?: Snippet;
    summary?: Snippet;
    children: Snippet;
    open?: boolean;
    class?: string;
  }
  let {
    title = "",
    facts = "",
    aside,
    summary,
    children,
    open = $bindable(false),
    class: surface = "",
  }: Props = $props();
</script>

<details class="disclosure {surface}" bind:open>
  <summary>
    <span class="disclosure-summary">
      {#if summary}
        {@render summary()}
      {:else}
        <span class="disclosure-title"
          ><span class="caps">{title}</span>{@render aside?.()}</span
        >
        {#if facts}<span class="disclosure-facts">{facts}</span>{/if}
      {/if}
    </span>
  </summary>
  <div class="disclosure-body">{@render children()}</div>
</details>
