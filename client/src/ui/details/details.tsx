// The Details sheet's body; this browser's address shows only while its key is pressed.
import "./details.css";
import type { App } from "../../app/app";
import { protocolFromNextHop } from "../../engine/net/paths";
import { describeStreams, feedSocket, needsPings } from "../../engine/net/streams";
import { activities } from "../../engine/run/schedule";
import type { Role } from "../../engine/types";
import { For, Show } from "../../lib/dom";
import { reducedMotion } from "../../lib/motion";
import { computed, effect, onCleanup, signal, untrack } from "../../lib/signal";
import { fmtLatency, MISSING } from "../../view/format";
import { PATH, PATH_TIP, READINESS, TIP, TRANSPORT, type Word } from "../../view/words";
import { announce } from "../kit/announce";
import { Icon } from "../kit/icon";
import { tipGroup } from "../kit/rove";
import { ServerScope } from "../kit/scope";
import { term, tip } from "../kit/tip";
import { serverIssues } from "../measure/cards-model";
import { liveOf } from "../measure/live";
import type { Shell } from "../shell/api";
import { describePath, pathSummary, shownTarget } from "../settings/paths";
import { capabilities, evidence, httpPaths, loadSummary } from "./facts";
import { diagnosticReport } from "./report";

const ROLES = ["throughput", "latency"] as const;
const COPIED_MS = 1_500;
// A release stamps the full commit; the row shows git's short form, its tip and the report the whole one.
const REVISION = /^[0-9a-f]{12,}$/.test(BUILD.revision)
  ? BUILD.revision.slice(0, 8)
  : BUILD.revision;

/** An address that breaks after each colon, so IPv6 wraps there. */
const breakable = (address: string) =>
  address
    .split(":")
    .flatMap((part, index) => (index ? [":", document.createElement("wbr"), part] : [part]));

export function DetailsPanel({ app, shell }: { app: App; shell: Shell }): Node {
  const { servers, run } = app;
  const live = liveOf(app);
  // While a run exists, the servers it prepared; otherwise the selected ones.
  const listed = computed(() => {
    const prepared = run.prepared();
    if (prepared.length) return prepared.map(({ server }) => server);
    const ids = servers.selected();
    const views = servers.views();
    return (servers.catalog()?.servers ?? [])
      .filter((entry) => ids.includes(entry.id))
      .map((entry) => views.get(entry.id)?.server ?? entry);
  });
  // Details follows the results lens, or the server whose latency shows, until someone inspects another here.
  const followed = computed(() => live.lens() || live.focus());
  const inspected = signal<string | null>(null);
  effect(() => {
    followed();
    untrack(() => inspected.set(null));
  });
  const shown = computed(() => {
    const list = listed();
    const find = (id: string | null) => list.find((server) => server.id === id);
    return find(inspected()) ?? find(followed()) ?? list[0] ?? null;
  });
  const shownId = computed(() => shown()?.id ?? null);
  // The address masks again whenever the sheet closes or another server shows.
  const revealed = signal(false);
  effect(() => {
    shell.shown("details");
    shownId();
    untrack(() => revealed.set(false));
  });

  const view = computed(() => {
    const id = shownId();
    return id === null ? undefined : servers.views().get(id);
  });
  const prepared = computed(
    () => run.prepared().find(({ server }) => server.id === shownId()) ?? null,
  );
  const mode = computed(() => (run.running() ? "running" : prepared() ? "result" : "live"));
  // What Start prepared while its run is shown, so nothing a later check learns moves it; else the live checks.
  const facts = computed(() => {
    const frozen = prepared();
    if (frozen)
      return {
        discovery: frozen.discovery,
        throughput: frozen.throughput,
        latency: frozen.latency,
      };
    const current = view();
    return {
      discovery: current?.discovery ?? null,
      throughput: current?.throughput.path ?? null,
      latency: current?.latency.path ?? null,
    };
  });
  const discovery = () => facts().discovery;
  /** The role's target: the prepared one, else what the server's checks resolve now. */
  const target = (role: Role) =>
    prepared() ? (facts()[role]?.target ?? null) : shownTarget(view(), role, app.page);
  const runConfig = computed(() => ({ ...app.preferences.config(), ...run.plan() }));
  const issues = computed(() => {
    const table = run.table();
    const id = shownId();
    return table && id !== null ? serverIssues(table, id) : [];
  });
  // A failed server's missing facts will not arrive.
  const unreachable = computed(() => !issues().length && view()?.readiness === "failed");
  const unknown = () => (unreachable() ? MISSING : "Pending");

  const latencyNeeded = computed(() => {
    const frozen = prepared();
    return frozen ? frozen.latency !== null : activities(runConfig()).some(needsPings);
  });
  const status = (role: Role): Word =>
    mode() === "running"
      ? READINESS["in-use"]
      : mode() === "result"
        ? READINESS.used
        : view()?.[role].path?.verifiedFor.fallback
          ? READINESS.fallback
          : READINESS[view()?.[role].state ?? "unchecked"];
  // Before a path is proven, the words assume HTTP/1.1.
  const streams = computed(() => {
    const { discovery, throughput } = facts();
    const server =
      throughput && discovery ? { server: { id: "", name: "" }, discovery, throughput } : null;
    return describeStreams(runConfig().transferStreams, activities(runConfig()), server);
  });
  const uploadFeed = () => {
    const shown = target("throughput");
    if (!shown) return unknown();
    const known = discovery();
    if (shown.transport !== "fetch-stream")
      return `${TRANSPORT.webtransport} over ${describePath(known, "throughput", shown).carrier}`;
    if (!known) return unknown();
    // A fetch upload's feed reaches the discovery origin, whichever origin carries its lanes.
    const socket = feedSocket(known);
    const { carrier } = describePath(known, "throughput", {
      transport: "fetch-stream",
      origin: known.origin,
      tls: known.origin.startsWith("https:"),
      protocol: socket ? "http1" : (protocolFromNextHop(known.protocol) ?? "negotiated"),
    });
    return `${TRANSPORT[socket ? "websocket" : "fetch-stream"]} over ${carrier}`;
  };
  const address = computed(() => {
    const probe = facts().throughput?.probe;
    return (
      probe && {
        ip: probe.clientIp,
        aside: `IPv${probe.clientIpVersion}, ${probe.clientIpSource === "forwarded" ? "trusted proxy" : "socket peer"}`,
      }
    );
  });
  // A failed server's last load reading no longer describes it.
  const load = computed(() =>
    issues().length || view()?.readiness === "failed"
      ? null
      : loadSummary(facts().throughput?.probe.load),
  );
  const supports = (items: string[] | null) =>
    items === null ? ["Checking server"] : items.length ? items : ["None advertised"];

  const copied = signal<"" | "copied" | "failed">("");
  let copiedTimer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(copiedTimer));
  // Everything the page holds about the servers and the run, read on demand.
  const report = () => {
    const views = servers.views.peek();
    const measured = run.details.peek();
    const probes = [...views.values()].flatMap((each) => [
      each.throughput.path?.probe,
      each.latency.path?.probe,
    ]);
    const latencyServer = servers.latencyServer.peek();
    const { unitKind, unitBase, showWireEstimates, resultHistoryPreference } = app.preferences;
    return diagnosticReport({
      page: location.href,
      servers: [
        ...(servers.catalog.peek()?.servers ?? []),
        ...[...views.values()].map((each) => each.server),
        ...(measured?.selection ?? []),
      ],
      clientIps: [...new Set(probes.flatMap((probe) => probe?.clientIp ?? []))],
      state: {
        client: {
          profile: BUILD.profile,
          version: BUILD.version,
          revision: BUILD.revision,
          clientVersion: BUILD.clientVersion,
          identity: `${BUILD.profile} ${BUILD.clientVersion}`,
        },
        environment: {
          userAgent: navigator.userAgent,
          secureContext: isSecureContext,
          webTransport: "WebTransport" in globalThis,
          online: navigator.onLine,
          viewport: `${innerWidth}x${innerHeight}@${devicePixelRatio}`,
          reducedMotion: reducedMotion.peek(),
        },
        scope: mode.peek(),
        inspected: shownId.peek(),
        preferences: {
          units: `${unitKind.peek()} ${unitBase.peek()}`,
          wireEstimates: showWireEstimates.peek(),
          history: resultHistoryPreference.peek(),
        },
        config: runConfig.peek(),
        selectedServers: servers.selected.peek(),
        servers: views,
        run: {
          phase: run.phase.peek(),
          preparation: run.preparation.peek(),
          startError: run.startError.peek(),
          connectivity: (latencyServer && views.get(latencyServer)?.connectivity) ?? null,
          stall: run.stall.peek(),
          stages: run.statuses.peek(),
          stageResults: run.results.peek(),
          result: run.result.peek(),
          servers: measured,
        },
      },
    });
  };
  // A plain-HTTP page has no Clipboard API: there the report is copied through a selection, within the press.
  async function copyReport(from: HTMLElement) {
    clearTimeout(copiedTimer);
    const text = report();
    let done: boolean;
    try {
      if (navigator.clipboard) await navigator.clipboard.writeText(text);
      done = !!navigator.clipboard || copyBySelection(text, from);
    } catch {
      done = false;
    }
    copied.set(done ? "copied" : "failed");
    copiedTimer = setTimeout(() => copied.set(""), COPIED_MS);
    announce(done ? "Diagnostic report copied." : "Could not copy the diagnostic report.");
  }

  const fact = (
    label: string,
    value: () => string,
    explain?: () => string,
    aside?: () => string,
  ) => (
    <div>
      <dt ref={explain && tip(explain)}>{label}</dt>
      <dd>
        {value}
        {aside && <span class="aside">{aside}</span>}
      </dd>
    </div>
  );
  const list = (label: string, items: () => string[]) => (
    <div>
      <dt>{label}</dt>
      <dd class="list">{() => items().map((item) => <span>{item}</span>)}</dd>
    </div>
  );

  return (
    <section class="infra">
      <div class="group server-card">
        <h3>Test server</h3>
        <dl class="kv" ref={tipGroup}>
          <Show when={() => listed().length > 1}>
            {() => (
              <div>
                <dt>Server</dt>
                <dd>
                  <ServerScope
                    label="Server"
                    servers={listed}
                    value={() => shownId() ?? ""}
                    onChange={(id) => inspected.set(id)}
                  />
                </dd>
              </div>
            )}
          </Show>
          {fact("Name", () => shown()?.name ?? "Checking server")}
          <Show when={shown}>{(server) => fact("Address", () => server().url)}</Show>
          <Show when={() => shown()?.location}>{(where) => fact("Location", where)}</Show>
          <Show when={load}>
            {(text) => (
              <div>
                <dt ref={term(() => TIP.serverLoad)}>Load</dt>
                <dd>{text}</dd>
              </div>
            )}
          </Show>
        </dl>
        <For each={issues} by={(issue) => `${issue.reason}\t${issue.stages}`}>
          {(issue) => (
            <p class="notice" data-tone="err">
              <strong>{() => issue().stages}</strong> {() => issue().reason}
            </p>
          )}
        </For>
        <Show
          when={() =>
            unreachable() ? (view()?.throughput.message ?? "Could not reach this test server") : ""
          }
        >
          {(text) => (
            <p class="notice" data-tone="err">
              {text}
            </p>
          )}
        </Show>
      </div>

      <div class="group">
        <h3>Connection</h3>
        <dl class="kv" ref={tipGroup}>
          {ROLES.map((role) => {
            const measured = () => role === "throughput" || latencyNeeded();
            return (
              <div class="path" data-role={role}>
                <dt ref={tip(() => PATH_TIP[role])}>{PATH[role]}</dt>
                <dd>
                  <span>
                    {() => {
                      if (!measured()) return mode() === "live" ? "Not selected" : "Not measured";
                      return pathSummary(discovery(), role, target(role));
                    }}
                  </span>
                  <Show when={measured}>
                    {() => (
                      <span class="path-status">
                        {() =>
                          status(role) !== READINESS.used && (
                            <span
                              class="status-dot inline"
                              data-tone={status(role).tone}
                              aria-hidden="true"
                            />
                          )
                        }
                        {() => status(role).label}
                      </span>
                    )}
                  </Show>
                </dd>
              </div>
            );
          })}
          <div>
            <dt ref={term(() => TIP.pathEvidence)}>Evidence</dt>
            <dd>
              {() => {
                const path = facts().throughput;
                return evidence(
                  path?.browserProtocol ?? null,
                  path?.probe.protocolNegotiated ?? null,
                  unknown(),
                );
              }}
            </dd>
          </div>
          {fact(
            "Streams",
            () => streams().value,
            () => TIP.forcedStreams,
            () => streams().aside,
          )}
          {fact("Upload feed", uploadFeed, () => TIP.uploadFeed)}
          <Show when={latencyNeeded}>
            {() =>
              fact(
                "Path check round trip",
                () => {
                  const rttMs = facts().latency?.rttMs;
                  return rttMs == null ? unknown() : fmtLatency(rttMs);
                },
                () => TIP.pathCheckRoundTrip,
              )
            }
          </Show>
          <div>
            <dt ref={tip(() => TIP.clientAddress)}>Your address</dt>
            <Show when={address} fallback={() => <dd>{unknown}</dd>}>
              {(known) => (
                <dd class="secret">
                  <span class="secret-text">
                    <Show
                      when={revealed}
                      fallback={() => (
                        <span class="secret-mask" aria-hidden="true">
                          ••••••••
                        </span>
                      )}
                    >
                      {() => <span class="secret-value">{() => breakable(known().ip)}</span>}
                    </Show>
                    <span class="aside">{() => known().aside}</span>
                  </span>
                  <button
                    class="btn btn-icon btn-quiet secret-key"
                    type="button"
                    aria-label="Show your address"
                    aria-pressed={() => String(revealed())}
                    ref={tip(() => `${revealed() ? "Hide" : "Show"} your address`)}
                    onclick={() => revealed.set(!revealed.peek())}
                  >
                    <Icon name="eye" />
                  </button>
                </dd>
              )}
            </Show>
          </div>
        </dl>
      </div>

      <div class="group">
        <h3>Server supports</h3>
        <dl class="kv" ref={tipGroup}>
          {list("HTTP", () => supports(httpPaths(discovery())))}
          {list("Throughput", () => supports(capabilities(discovery(), "throughput")))}
          {list("Latency", () => supports(capabilities(discovery(), "latency")))}
        </dl>
      </div>

      <div class="group">
        <h3>Build</h3>
        <dl class="kv" ref={tipGroup}>
          {fact(
            "Client",
            () => (BUILD.version ? `v${BUILD.version}` : REVISION),
            () =>
              REVISION === BUILD.revision ? TIP.client : `${TIP.client}\nCommit ${BUILD.revision}`,
            () => [BUILD.profile, BUILD.version && REVISION].filter(Boolean).join(", "),
          )}
          {fact(
            "Server",
            () => discovery()?.engineVersion ?? MISSING,
            () => {
              const generation = discovery()?.generation;
              return generation ? `${TIP.serverInstance}\nInstance ${generation}` : "";
            },
          )}
        </dl>
      </div>

      <div class="kv">
        <button
          class="link-row copy"
          type="button"
          onclick={(event) => void copyReport(event.currentTarget)}
        >
          <span class={() => (copied() ? "hidden" : false)}>Copy diagnostic report</span>
          <span
            class={() => (copied() === "copied" ? false : "hidden")}
            aria-hidden={() => String(copied() !== "copied")}
          >
            Copied
          </span>
          <span
            class={() => (copied() === "failed" ? false : "hidden")}
            aria-hidden={() => String(copied() !== "failed")}
          >
            Could not copy
          </span>
          {() => <Icon name={copied() === "copied" ? "check" : "copy"} />}
        </button>
        <button
          class="link-row"
          type="button"
          onclick={(event) => shell.legal(event.currentTarget)}
        >
          {"About & legal"}
          <Icon name="chevron" />
        </button>
      </div>
    </section>
  );
}

/** Copies `text` by selecting it in a field beside `at`, inside any dialog `at` is in; whether it copied. */
function copyBySelection(text: string, at: HTMLElement): boolean {
  const field = document.createElement("textarea");
  field.value = text;
  field.readOnly = true;
  field.setAttribute("aria-hidden", "true");
  Object.assign(field.style, {
    position: "fixed",
    top: "0",
    left: "0",
    opacity: "0",
    pointerEvents: "none",
  });
  at.after(field);
  field.select();
  let done = false;
  try {
    done = document.execCommand("copy");
  } catch {
    done = false;
  }
  field.remove();
  at.focus({ preventScroll: true });
  return done;
}
