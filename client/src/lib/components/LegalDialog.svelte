<script lang="ts">
  import Dialog from "./Dialog.svelte";
  import { loadLegal, retryLegal } from "../legal/loader";
  import type { LegalAbout, LegalComponent } from "../legal/types";

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

  function componentTitle(component: LegalComponent): string {
    return `${component.name} (${component.ecosystem})`;
  }

  $effect(() => {
    if (open) load();
  });
</script>

<!-- The History shortcut stays live so the archive can open over legal. -->
<Dialog
  {open}
  onCancel={onClose}
  labelledby="legal-dialog-title"
  lightDismiss
  shortcuts={["h"]}
  --dialog-width="880px"
  --dialog-height="min(86svh, 760px)"
>
  <header class="surface-head legal-head">
    <h2 id="legal-dialog-title">About &amp; legal</h2>
    <button class="btn btn-inset" type="button" onclick={onClose}>Close</button>
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
      <section
        class="surface-inset project-legal"
        aria-labelledby="project-legal-title"
      >
        <h3 class="caps" id="project-legal-title">{data.project.name}</h3>
        <p class="copyright">
          Copyright © {data.project.copyrightYears}
          {data.project.copyrightHolder}
        </p>
        <p>{data.project.licenseExpression}</p>
        <p>
          Graphite Meter is free software. It comes with absolutely no warranty,
          to the extent permitted by applicable law.
        </p>
        <p class="legal-links">
          <a href={data.sourceURL} target="_blank" rel="noopener noreferrer"
            >Source code</a
          >
          <a href={data.licenseURL} target="_blank" rel="noopener noreferrer"
            >Project license</a
          >
          <a href={data.noticesURL} target="_blank" rel="noopener noreferrer"
            >Third-party notices</a
          >
        </p>
      </section>

      <section aria-labelledby="third-party-title">
        <h3 class="caps" id="third-party-title">Third-party software</h3>
        {#each data.components as component (component.ecosystem + component.name + component.version)}
          <article class="component">
            <details class="surface disclosure" open={true}>
              <summary>{componentTitle(component)}</summary>
              <dl class="kv">
                <div>
                  <dt>Version</dt>
                  <dd>{component.version}</dd>
                </div>
                <div>
                  <dt>License</dt>
                  <dd>{component.selectedLicenseExpression}</dd>
                </div>
                <div>
                  <dt>Source</dt>
                  <dd>
                    <a
                      href={component.source}
                      target="_blank"
                      rel="noopener noreferrer">{component.source}</a
                    >
                  </dd>
                </div>
                <div>
                  <dt>Modified by Graphite Meter</dt>
                  <dd>{component.modified ? "yes" : "no"}</dd>
                </div>
              </dl>
            </details>
          </article>
        {/each}
      </section>
    {/if}
  </div>
</Dialog>

<style>
  .legal-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
    padding: var(--space-3) var(--space-4);
  }
  h2 {
    font: var(--w-strong) var(--type-lg) var(--font-display);
    letter-spacing: var(--track-tight);
  }
  .legal-body {
    min-height: 0;
    overflow-y: auto;
    overscroll-behavior: contain;
    padding: var(--space-5);
    color: var(--text-muted);
    font-size: var(--type-sm);
    line-height: 1.55;
  }
  section + section {
    margin-top: var(--space-5);
  }
  h3 {
    color: var(--brand-strong);
  }
  .project-legal {
    padding: var(--space-4);
  }
  .project-legal p {
    margin-top: var(--space-2);
  }
  .copyright {
    color: var(--text);
    font: var(--type-xs) var(--font-mono);
  }
  .legal-links {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2) var(--space-4);
  }
  a {
    color: var(--brand-strong);
    text-decoration: underline;
    text-underline-offset: 2px;
  }
  .component {
    margin-top: var(--space-2);
  }
  summary {
    padding: var(--space-2) var(--space-3);
    color: var(--text);
  }
  .kv {
    --kv-label: 190px;
    gap: 2px;
    padding: var(--space-2) var(--space-3) var(--space-3);
    border-top: 1px solid var(--border-subtle);
    font-family: var(--font-mono);
  }
  .kv :is(dt, dd) {
    font-size: var(--type-xs);
  }
  .legal-status {
    display: grid;
    place-items: start;
    gap: var(--space-3);
    min-height: 10rem;
  }
  @media (max-width: 759px) {
    .legal-body {
      padding: var(--space-3);
    }
    .project-legal {
      padding: var(--space-3);
    }
  }
</style>
