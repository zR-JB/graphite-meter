<script lang="ts">
  let theme = $state<"dark" | "light">("dark");

  function toggleTheme() {
    theme = theme === "dark" ? "light" : "dark";
    document.documentElement.setAttribute("data-theme", theme);
  }
</script>

<main
  class="grid h-dvh place-items-center bg-bg text-text"
  style="font-family: var(--font-sans)"
>
  <div
    class="rounded-[var(--radius-xl)] border border-border bg-surface-1 p-8 shadow-[var(--shadow-card)]"
  >
    <h1 class="text-2xl font-bold tracking-tight">The Graphite Meter</h1>
    <p class="mt-2 text-muted">
      Project scaffold ready — tokens, Tailwind bridge, Svelte 5, Vite.
    </p>

    <div class="mt-6 flex items-center gap-2">
      <span class="h-4 w-4 rounded-[var(--radius-xs)]" style="background: var(--brand)"></span>
      <span class="h-4 w-4 rounded-[var(--radius-xs)]" style="background: var(--signal)"></span>
      <span class="h-4 w-4 rounded-[var(--radius-xs)]" style="background: var(--ok)"></span>
      <span class="h-4 w-4 rounded-[var(--radius-xs)]" style="background: var(--warn)"></span>
      <span class="h-4 w-4 rounded-[var(--radius-xs)]" style="background: var(--err)"></span>
    </div>

    <button
      onclick={toggleTheme}
      class="mt-6 rounded-[var(--radius-md)] border border-border bg-surface-2 px-4 py-2
             font-mono text-sm transition-colors hover:border-border-strong"
      style="font-family: var(--font-mono)"
    >
      theme: {theme}
    </button>
  </div>
</main>
