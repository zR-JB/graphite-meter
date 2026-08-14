<script lang="ts">
  import { focusTrap } from "../actions/focusTrap";
  import { loadLegal, retryLegal } from "../legal/loader";
  import type { LegalAbout, LegalComponent } from "../legal/types";

  interface Props {
    open: boolean;
    invoker?: HTMLElement | null;
    onClose: () => void;
  }

  let { open, invoker = null, onClose }: Props = $props();
  let loadState = $state<"loading" | "ready" | "error">("loading");
  let data = $state<LegalAbout | null>(null);

  function load() {
    loadState = "loading";
    void loadLegal()
      .then((value) => {
        data = value;
        loadState = "ready";
      })
      .catch(() => {
        loadState = "error";
      });
  }

  function retry() {
    loadState = "loading";
    void retryLegal()
      .then((value) => {
        data = value;
        loadState = "ready";
      })
      .catch(() => {
        loadState = "error";
      });
  }

  function close() {
    onClose();
    window.setTimeout(() => invoker?.focus(), 0);
  }

  function onDialogKeydown(event: KeyboardEvent) {
    event.stopPropagation();
    if (event.key === "Escape") {
      event.preventDefault();
      close();
    }
  }

  function componentTitle(component: LegalComponent): string {
    return `${component.name} (${component.ecosystem})`;
  }

  $effect(() => {
    if (open) load();
  });

  $effect(() => {
    if (!open) return;
    const previousOverflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    return () => {
      document.body.style.overflow = previousOverflow;
    };
  });
</script>

{#if open}
  <div
    class="legal-backdrop"
    role="presentation"
    onclick={(event) => {
      if (event.target === event.currentTarget) close();
    }}
  >
    <div
      class="legal-dialog"
      role="dialog"
      aria-modal="true"
      aria-labelledby="legal-dialog-title"
      tabindex="-1"
      use:focusTrap={true}
      onkeydown={onDialogKeydown}
    >
      <header class="legal-head">
        <h2 id="legal-dialog-title">About &amp; legal</h2>
        <button class="close-btn" type="button" onclick={close}>Close</button>
      </header>

      <div class="legal-body">
        {#if loadState === "loading"}
          <p class="legal-status" role="status">Loading legal notices…</p>
        {:else if loadState === "error"}
          <div class="legal-status" role="alert">
            <p>Unable to load legal notices.</p>
            <button type="button" class="retry-btn" onclick={retry}
              >Retry</button
            >
          </div>
        {:else if data}
          <section class="project-legal" aria-labelledby="project-legal-title">
            <h3 id="project-legal-title">{data.project.name}</h3>
            <p class="copyright">
              Copyright © {data.project.copyrightYears}
              {data.project.copyrightHolder}
            </p>
            <p>{data.project.licenseExpression}</p>
            <p>
              Graphite Meter is free software. It comes with absolutely no
              warranty, to the extent permitted by applicable law.
            </p>
            <p class="legal-links">
              <a href={data.sourceURL} target="_blank" rel="noopener noreferrer"
                >Source code</a
              >
              <a
                href={data.licenseURL}
                target="_blank"
                rel="noopener noreferrer">Project license</a
              >
              <a href={data.noticesURL}>Third-party notices</a>
            </p>
          </section>

          <section aria-labelledby="third-party-title">
            <h3 id="third-party-title">Third-party software</h3>
            {#each data.components as component (component.ecosystem + component.name + component.version)}
              <article class="component">
                <details>
                  <summary>{componentTitle(component)}</summary>
                  <dl>
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
    </div>
  </div>
{/if}

<style>
  .legal-backdrop {
    position: fixed;
    inset: 0;
    z-index: 220;
    display: grid;
    place-items: center;
    padding: var(--space-4);
    background: color-mix(in srgb, var(--canvas) 70%, transparent);
    overscroll-behavior: contain;
  }
  .legal-dialog {
    display: flex;
    flex-direction: column;
    width: min(880px, calc(100vw - 2 * var(--space-4)));
    max-height: min(86svh, 760px);
    min-height: 0;
    border: 1px solid var(--border-strong);
    border-radius: var(--r-chrome);
    background: var(--surface-1);
    box-shadow: var(--shadow-float);
  }
  .legal-head {
    position: sticky;
    top: 0;
    z-index: 1;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
    flex: 0 0 auto;
    padding: var(--space-4);
    border-bottom: 1px solid var(--border);
    background: var(--surface-1);
  }
  .legal-head h2,
  .legal-body h3 {
    margin: 0;
    font-family: var(--font-display);
  }
  .legal-head h2 {
    font-size: var(--type-lg);
  }
  .legal-body {
    min-height: 0;
    overflow-y: auto;
    overscroll-behavior: contain;
    padding: var(--space-4);
    line-height: 1.45;
  }
  .legal-body section + section {
    margin-top: var(--space-6);
  }
  .legal-body h3 {
    font-size: var(--type-md);
  }
  .copyright {
    margin: var(--space-1) 0;
  }
  .legal-links {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-4);
  }
  .legal-links a,
  .component a {
    color: var(--brand);
  }
  .component {
    padding-top: var(--space-2);
  }
  .component details {
    border-top: 1px solid var(--border);
    padding-top: var(--space-2);
  }
  .component summary {
    cursor: pointer;
    font-family: var(--font-display);
    font-size: var(--type-sm);
    font-weight: 650;
  }
  .component dl {
    margin: var(--space-2) 0 0;
  }
  .component dl div {
    display: grid;
    grid-template-columns: minmax(120px, 0.3fr) 1fr;
    gap: var(--space-2);
  }
  .component dt {
    color: var(--text-soft);
  }
  .component dd {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .legal-status {
    display: grid;
    place-items: start;
    gap: var(--space-3);
    min-height: 10rem;
  }
  .close-btn,
  .retry-btn {
    min-height: 32px;
    padding: 0 var(--space-3);
    border: 1px solid var(--border);
    border-radius: var(--r-chrome);
    background: var(--surface-2);
    color: var(--text);
    font-weight: 650;
  }
  .close-btn:focus-visible,
  .retry-btn:focus-visible,
  a:focus-visible {
    outline: var(--focus-ring);
    outline-offset: 2px;
  }
  @media (prefers-reduced-motion: reduce) {
    .legal-backdrop,
    .legal-dialog {
      animation: none;
      transition: none;
    }
  }
  @media (max-width: 560px) {
    .legal-backdrop {
      padding: var(--space-2);
    }
    .legal-dialog {
      width: calc(100vw - 2 * var(--space-2));
      max-height: 90svh;
    }
    .legal-body {
      padding: var(--space-3);
    }
    .component dl div {
      grid-template-columns: 1fr;
      gap: 0;
    }
  }
</style>
