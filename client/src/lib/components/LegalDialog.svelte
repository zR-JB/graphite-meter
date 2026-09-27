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

      <section class="group" aria-labelledby="third-party-title">
        <h3 id="third-party-title">Third-party software</h3>
        <dl class="kv components">
          {#each data.components as component (component.ecosystem + component.name + component.version)}
            <div class="component">
              <dt>{component.name}</dt>
              <dd>
                {component.ecosystem} · {component.version} · {component.selectedLicenseExpression}
                · {component.modified
                  ? "Modified by Graphite Meter"
                  : "Unmodified"}
                <a
                  href={component.source}
                  target="_blank"
                  rel="noopener noreferrer">{component.source}</a
                >
              </dd>
            </div>
          {/each}
        </dl>
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
    padding: var(--space-3) var(--space-4);
  }
  h2 {
    font: var(--w-strong) var(--type-lg) var(--font-display);
    letter-spacing: var(--track-tight);
  }
  .legal-body {
    display: grid;
    gap: var(--space-4);
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
  .components a {
    display: block;
    width: fit-content;
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
  }
  /* Component names, not field labels: the column fits a module path. */
  .components {
    --kv-label: 17rem;
  }
</style>
