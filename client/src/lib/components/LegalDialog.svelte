<script lang="ts">
  import { untrack } from "svelte";
  import Dialog from "./Dialog.svelte";
  import Icon from "./Icon.svelte";
  import { loadLegal, retryLegal } from "../legal/loader";
  import type { LegalAbout } from "../legal/types";

  interface Props {
    open: boolean;
    onClose: () => void;
  }

  let { open, onClose }: Props = $props();
  let loadState = $state<"loading" | "ready" | "error">("loading");
  let data = $state.raw<LegalAbout | null>(null);

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
    if (open)
      untrack(() => {
        if (!data) load();
      });
  });

  const ECOSYSTEM: Record<string, string> = {
    go: "Go modules",
    "go-toolchain": "Go toolchain",
    cargo: "Rust crates",
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
  class="float sheet legal-dialog"
  labelledby="legal-dialog-title"
  lightDismiss
>
  <header class="sheet-head">
    <!-- svelte-ignore a11y_autofocus -->
    <h2 id="legal-dialog-title" tabindex="-1" autofocus>About &amp; legal</h2>
    <div class="head-actions">
      <button
        class="btn btn-icon btn-quiet"
        type="button"
        aria-label="Close About & legal"
        onclick={onClose}><Icon name="close" /></button
      >
    </div>
  </header>

  <div class="sheet-body legal-body">
    {#if loadState === "loading"}
      <p class="legal-status" role="status">Loading legal notices…</p>
    {:else if loadState === "error"}
      <div class="legal-status" role="alert">
        <p>Unable to load legal notices.</p>
        <button type="button" class="btn" onclick={() => load(retryLegal)}
          >Retry</button
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
        <div class="kv">
          {#each [["Source code", data.sourceURL], ["Project license", data.licenseURL], ["Third-party notices", data.noticesURL]] as [label, href] (label)}
            <a class="link-row" {href} target="_blank" rel="noopener noreferrer"
              >{label}<Icon name="external" /></a
            >
          {/each}
        </div>
      </section>

      <section class="third-party" aria-labelledby="third-party-title">
        <div class="group-head">
          <h3 id="third-party-title">Third-party software</h3>
        </div>
        {#each ecosystems as [ecosystem, components] (ecosystem)}
          <section class="group" aria-label={ECOSYSTEM[ecosystem] ?? ecosystem}>
            <h4 class="list-label">{ECOSYSTEM[ecosystem] ?? ecosystem}</h4>
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
                    <span class="component-links">
                      {#each component.links as link (link.label)}
                        <a
                          href={link.url}
                          aria-label={`${link.label} for ${component.name}`}
                          target="_blank"
                          rel="noopener noreferrer"
                          >{link.label}<Icon name="external" /></a
                        >
                      {/each}
                    </span>
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
    --panel-pad: var(--space-5);
  }
  .legal-body {
    display: grid;
    align-content: start;
    gap: var(--space-6);
  }
  .group > .kv + .kv {
    margin-top: var(--space-2);
  }
  .third-party {
    display: grid;
    gap: var(--space-4);
  }
  .third-party > .group-head {
    padding-inline: var(--row-inset);
  }
  /* Component names, not field labels: the column fits a module path. */
  .components {
    --kv-label: 16rem;
  }
  .components dd {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr) minmax(0, 2fr);
    gap: 2px var(--space-3);
  }
  .component-links {
    display: flex;
    flex-wrap: wrap;
    align-items: start;
    gap: 0 var(--space-3);
  }
  .components a {
    display: inline-flex;
    align-items: center;
    gap: var(--space-1);
    color: var(--text-muted);
    text-decoration-color: transparent;
    text-underline-offset: 0.2em;
    white-space: nowrap;
  }
  .components a :global(svg) {
    flex: none;
    width: var(--icon-sm);
    height: var(--icon-sm);
  }
  @media (hover: hover) {
    .components a:hover {
      color: var(--brand-strong);
      text-decoration-color: currentColor;
    }
  }
  .legal-status {
    display: grid;
    place-items: start;
    gap: var(--space-3);
    min-height: 10rem;
    padding-inline: var(--row-inset);
    color: var(--text-muted);
    font: var(--role-row);
  }
  @media (max-width: 759px) {
    :global(dialog.legal-dialog) {
      --panel-pad: var(--space-3);
    }
    .components dd {
      grid-template-columns: auto minmax(0, 1fr);
    }
    .component-links {
      grid-column: 1 / -1;
    }
  }
</style>
