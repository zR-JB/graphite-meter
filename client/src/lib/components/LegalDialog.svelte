<script lang="ts">
  import Dialog from "./Dialog.svelte";
  import Icon from "./Icon.svelte";
  import { tooltip } from "../actions/tooltip";
  import { loadLegal, retryLegal } from "../legal/loader";
  import type { LegalAbout } from "../legal/types";

  interface Props {
    open: boolean;
    onClose: () => void;
  }

  let { open, onClose }: Props = $props();
  let loadState = $state<"loading" | "ready" | "error">("loading");
  let data = $state<LegalAbout | null>(null);

  function load(request = loadLegal) {
    loadState = "loading";
    void request()
      .then((value) => {
        data = value;
        loadState = "ready";
      })
      .catch(() => {
        loadState = "error";
      });
  }

  $effect(() => {
    if (open) load();
  });

  const ECOSYSTEM: Record<string, string> = {
    go: "Go modules",
    "go-toolchain": "Go toolchain",
    npm: "npm packages",
    font: "Fonts",
  };
  const ecosystems = $derived([
    ...Map.groupBy(data?.components ?? [], (component) => component.ecosystem),
  ]);
</script>

<Dialog
  {open}
  onCancel={onClose}
  class="float legal-dialog"
  labelledby="legal-dialog-title"
  lightDismiss
>
  <header class="surface-head legal-head">
    <h2 id="legal-dialog-title">About &amp; legal</h2>
    <button
      class="btn btn-icon btn-inset"
      type="button"
      aria-label="Close"
      {@attach tooltip(() => "Close (Esc)")}
      onclick={onClose}><Icon name="close" /></button
    >
  </header>

  <div class="legal-body">
    {#if loadState === "loading"}
      <p class="legal-status" role="status">Loading legal notices…</p>
    {:else if loadState === "error"}
      <div class="legal-status" role="alert">
        <p>Unable to load legal notices.</p>
        <button
          type="button"
          class="btn btn-inset"
          onclick={() => load(retryLegal)}>Retry</button
        >
      </div>
    {:else if data}
      <section class="group" aria-labelledby="project-legal-title">
        <h3 id="project-legal-title">{data.project.name}</h3>
        <dl class="kv">
          <div>
            <dt>Copyright</dt>
            <dd>
              © {data.project.copyrightYears}
              {data.project.copyrightHolder}
            </dd>
          </div>
          <div>
            <dt>License</dt>
            <dd>{data.project.licenseExpression}</dd>
          </div>
          <div>
            <dt>Warranty</dt>
            <dd>
              Graphite Meter is free software. It comes with absolutely no
              warranty, to the extent permitted by applicable law.
            </dd>
          </div>
        </dl>
        <p class="legal-links">
          <a
            class="btn"
            href={data.sourceURL}
            target="_blank"
            rel="noopener noreferrer">Source code</a
          >
          <a
            class="btn"
            href={data.licenseURL}
            target="_blank"
            rel="noopener noreferrer">Project license</a
          >
          <a
            class="btn"
            href={data.noticesURL}
            target="_blank"
            rel="noopener noreferrer">Third-party notices</a
          >
        </p>
      </section>

      <section class="third-party" aria-labelledby="third-party-title">
        <h3 id="third-party-title">Third-party software</h3>
        {#each ecosystems as [ecosystem, components] (ecosystem)}
          <section class="group" aria-label={ECOSYSTEM[ecosystem] ?? ecosystem}>
            <h3>{ECOSYSTEM[ecosystem] ?? ecosystem}</h3>
            <dl class="kv components">
              {#each components as component (component.name + component.version)}
                <div class="component">
                  <dt>{component.name}</dt>
                  <dd>
                    <span
                      >{component.version}{#if component.modified}<span
                          class="aside">Modified</span
                        >{/if}</span
                    >
                    <span>{component.selectedLicenseExpression}</span>
                    <a
                      href={component.source}
                      target="_blank"
                      rel="noopener noreferrer"
                      >{component.source.replace(/^https?:\/\//, "")}</a
                    >
                  </dd>
                </div>
              {/each}
            </dl>
          </section>
        {/each}
      </section>
    {/if}
  </div>
</Dialog>

<style>
  :global(dialog.legal-dialog) {
    --dialog-width: 880px;
    --dialog-height: min(86svh, 760px);
  }
  .legal-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
    min-height: 56px;
    padding: var(--space-2) var(--space-4);
  }
  h2 {
    font: var(--w-strong) var(--type-lg) var(--font-display);
    letter-spacing: var(--track-tight);
  }
  .legal-body {
    display: grid;
    align-content: start;
    gap: var(--space-5);
    min-height: 0;
    overflow-y: auto;
    overscroll-behavior: contain;
    padding: var(--space-5);
  }
  .legal-links {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }
  .third-party {
    display: grid;
    gap: var(--space-3);
  }
  .third-party > h3 {
    padding-inline: var(--space-3);
    font: var(--w-strong) var(--type-md) / 1.3 var(--font-sans);
  }
  .components dd {
    display: grid;
    grid-template-columns: minmax(0, 1.2fr) minmax(0, 1fr) minmax(0, 1.8fr);
    gap: 2px var(--space-3);
  }
  .components a {
    width: fit-content;
    overflow-wrap: anywhere;
  }
  .aside {
    margin-left: var(--space-2);
    color: var(--text-soft);
  }
  .legal-status {
    display: grid;
    place-items: start;
    gap: var(--space-3);
    min-height: 10rem;
    color: var(--text-muted);
    font-size: var(--type-sm);
  }
  @media (max-width: 759px) {
    .legal-body {
      padding: var(--space-3);
    }
    .components dd {
      grid-template-columns: auto minmax(0, 1fr);
    }
    .components a {
      grid-column: 1 / -1;
    }
  }
  /* Component names, not field labels: the column fits a module path. */
  .components {
    --kv-label: 16rem;
  }
</style>
