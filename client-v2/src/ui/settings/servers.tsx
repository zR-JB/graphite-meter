// The Test servers checklist and what stands in its way: a catalogue that did not load, a saved selection that
// changed, and each selected server's problem with its Retry or Sign in.
import type { App } from "../../app/app";
import type { Approval } from "../../engine/net/servers";
import type { CatalogEntry } from "../../protocol/documents";
import { For, Show } from "../../lib/dom";
import { computed, onCleanup, signal, type Read } from "../../lib/signal";
import { fmtMs } from "../../view/format";
import { READINESS, serverLabel } from "../../view/words";
import { clip, tip } from "../kit/tip";
import { preflightNote, TIP } from "./tips";

/** The most servers a test runs on. */
const MAX_SERVERS = 4;

export function ServerSelection(props: { app: App; locked: Read<boolean> }): Node {
  const { servers } = props.app;
  const { locked } = props;
  const entries = computed(() => servers.catalogue()?.servers ?? []);
  const retrying = signal<ReadonlySet<string>>(new Set());
  const rows = new Map<string, HTMLLabelElement>();
  /** One server's view, which keeps its identity while another server's changes. */
  const viewOf = (id: string) => computed(() => servers.views().get(id));
  /** As its discovery names it, else as the catalogue does. */
  const identity = (entry: Read<CatalogEntry>, view: ReturnType<typeof viewOf>) =>
    computed(() => view()?.server ?? entry());

  // The list states a sign-in, and with several servers a failure; a single failed server's paths speak for it.
  const problems = computed(() => {
    const selected = servers.selected();
    const several = selected.length > 1;
    const views = servers.views();
    const busy = retrying();
    return entries().filter(({ id }) => {
      const readiness = views.get(id)?.readiness;
      return (
        selected.includes(id) && (readiness === "sign-in" || (several && (readiness === "failed" || busy.has(id))))
      );
    });
  });

  async function retry(id: string, button: HTMLButtonElement) {
    if (locked.peek() || retrying.peek().has(id)) return;
    retrying.set(new Set([...retrying.peek(), id]));
    try {
      await servers.retry(id);
    } finally {
      // The key leaves with the problem; focus stays on the server it fixed.
      if (servers.views.peek().get(id)?.readiness === "verified" && document.activeElement === button) {
        const row = rows.get(id);
        (row?.querySelector<HTMLInputElement>("input:not(:disabled)") ?? row)?.focus({ preventScroll: true });
      }
      retrying.set(new Set([...retrying.peek()].filter((each) => each !== id)));
    }
  }

  const choice = (entry: Read<CatalogEntry>) => {
    const { id } = entry.peek();
    const view = viewOf(id);
    const server = identity(entry, view);
    const checked = computed(() => servers.selected().includes(id));
    const readiness = computed(() => view()?.readiness);
    const status = computed(() => {
      const state = readiness();
      return !state || state === "unchecked" || (state === "verified" && !checked()) ? null : state;
    });
    const preflightMs = computed(() => view()?.discovery?.requestMs);
    const preflight = computed(() => {
      const ms = preflightMs();
      const state = readiness();
      return ms === undefined || state === "failed" || state === "sign-in" || state === "checking" ? "" : fmtMs(ms);
    });
    return (
      <label
        tabindex="-1"
        class={() => (checked() ? "checked" : false)}
        ref={(row) => {
          rows.set(id, row);
          onCleanup(() => rows.delete(id));
          tip(() => {
            const { name, url, location } = server();
            const ms = preflightMs();
            return [name, new URL(url).host, location, ms === undefined ? "" : preflightNote(fmtMs(ms))]
              .filter(Boolean)
              .join("\n");
          })(row);
        }}
      >
        <input
          class="check"
          type="checkbox"
          checked={checked}
          disabled={() => {
            const count = servers.selected().length;
            return locked() || (checked() ? count === 1 : count >= MAX_SERVERS);
          }}
          aria-describedby={() => (preflight() ? "settings-preflight-note" : undefined)}
          onchange={(event) => {
            // The row shows what the selection keeps.
            event.currentTarget.checked = checked.peek();
            const selected = servers.selected.peek();
            servers.apply(checked.peek() ? selected.filter((each) => each !== id) : [...selected, id]);
          }}
        />
        <span class="server-name" ref={clip()}>
          {() => server().name}
          {() => (serverLabel(server()) === server().name ? null : <small>{server().location}</small>)}
        </span>
        <Show when={status}>
          {(state) => (
            <small class="server-status" data-state={state}>
              <span class="status-dot inline" data-tone={() => READINESS[state()].tone} />
              {() => READINESS[state()].label}
            </small>
          )}
        </Show>
        <Show when={preflight}>
          {(ms) => (
            <small class="server-preflight">
              <span class="sr-only">Preflight request </span>
              {ms}
              <span class="unit">ms</span>
            </small>
          )}
        </Show>
      </label>
    );
  };

  const problem = (entry: Read<CatalogEntry>) => {
    const { id } = entry.peek();
    const view = viewOf(id);
    const server = identity(entry, view);
    const name = () => server().name;
    const pending = computed(() => retrying().has(id));
    const signIn = computed(() => view()?.readiness === "sign-in");
    const approval = computed((): Exclude<Approval, { state: "idle" }> | null => {
      const current = servers.approval();
      return current.state !== "idle" && current.id === id ? current : null;
    });
    const waiting = computed(() => {
      const state = approval()?.state;
      return state === "preparing" || state === "waiting";
    });
    return (
      <div class="server-feedback" role="status">
        <div>
          <strong>{name}</strong>
          {() => server().location && <small>{server().location}</small>}
        </div>
        <p class="feedback-message">
          {() => {
            // A retry's message rises in, and so does the outcome that replaces it.
            const checking = pending();
            return (
              <span class="enter">
                {() =>
                  checking
                    ? `Checking ${name()}…`
                    : signIn()
                      ? "Admits signed-in clients only"
                      : (view()?.message ?? "")
                }
              </span>
            );
          }}
        </p>
        {() =>
          signIn() ? (
            <button
              class="btn"
              type="button"
              disabled={() => locked() || waiting()}
              aria-label={() => `Sign in to ${name()}`}
              // The sign-in window must open within this click.
              onclick={() => servers.signIn(id)}
            >
              {() => (waiting() ? "Waiting for approval…" : "Sign in")}
            </button>
          ) : (
            <button
              class="btn"
              type="button"
              disabled={locked}
              aria-disabled={() => String(pending())}
              aria-busy={() => String(pending())}
              aria-label={() => `Retry ${name()}`}
              onclick={(event) => void retry(id, event.currentTarget)}
            >
              {() => (pending() ? READINESS.checking.label : "Retry")}
            </button>
          )
        }
        <Show when={approval}>
          {(flow) => [
            () => {
              const current = flow();
              if (current.state === "limit")
                return (
                  <p>
                    <a href={current.renewUrl} target="_blank" rel="noopener noreferrer">
                      Renew login at {name}
                    </a>
                    . Renewing ends the other client connections authorized by that login. Then choose Sign in again
                    here.
                  </p>
                );
              return (
                "code" in current && (
                  <p class="approval-code">
                    Compare this code with the sign-in page:{" "}
                    <strong aria-label={`Verification code ${current.code}`}>{current.code}</strong> Approve only if
                    both codes match.
                  </p>
                )
              );
            },
            <button class="btn" type="button" onclick={() => servers.cancelSignIn()}>
              Cancel sign-in
            </button>,
            () => {
              const current = flow();
              return (
                "url" in current && (
                  <p>
                    <a href={current.url} target="_blank" rel="noopener noreferrer">
                      Open sign-in page
                    </a>{" "}
                    if no window opened, then return here.
                    {current.state === "failed" ? " Approval did not finish. Try Sign in again." : ""}
                  </p>
                )
              );
            },
          ]}
        </Show>
      </div>
    );
  };

  return (
    <>
      <Show when={() => entries().length > 1}>
        {() => (
          <div class="servers">
            <div class="list-label">
              <span id="settings-servers-label" ref={tip(() => TIP.testServers)}>
                Test servers
              </span>
              <small>{() => `${servers.selected().length} selected, up to ${MAX_SERVERS}`}</small>
            </div>
            <div class="kv">
              <div
                class="choices"
                role="group"
                aria-labelledby="settings-servers-label"
                aria-busy={() => String(servers.metadataLoading())}
              >
                <For each={entries} by={(entry) => entry.id}>
                  {choice}
                </For>
              </div>
            </div>
            <span class="sr-only" id="settings-preflight-note">
              Preflight request times include connection setup and the response. They are not latency measurements.
            </span>
          </div>
        )}
      </Show>
      <Show when={() => servers.unresolved().length > 0}>
        {() => (
          <div class="selection-notice" role="status">
            <p>The saved selection has changed</p>
            <button
              class="btn"
              type="button"
              disabled={locked}
              onclick={() => {
                const selected = servers.selected.peek();
                servers.apply(selected.length ? selected : ["self"]);
              }}
            >
              Use available servers
            </button>
          </div>
        )}
      </Show>
      <Show when={() => !servers.catalogue()}>
        {() => (
          <div class="selection-notice" role="status">
            <p>{() => (servers.loading() ? "Loading servers…" : "Could not load servers")}</p>
            <button
              class="btn"
              type="button"
              disabled={() => locked() || servers.loading()}
              onclick={() => void servers.retryCatalogue()}
            >
              Retry servers
            </button>
          </div>
        )}
      </Show>
      <For each={problems} by={(entry) => entry.id}>
        {problem}
      </For>
    </>
  );
}
