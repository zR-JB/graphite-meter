// One saved result as the console showed it: the console's cards and latency lanes in the result's own unit tier, then
// each server, how it was reached, its probe outcomes and the builds that measured it.
import type { App } from "../../app/app";
import type { HistoryRecord } from "../../app/history/records";
import type { LatencySummary } from "../../engine/measure/latency";
import { movedBytes } from "../../engine/run/result";
import { STAGES } from "../../engine/run/schedule";
import type { Role } from "../../engine/types";
import { For, Show } from "../../lib/dom";
import { computed, signal } from "../../lib/signal";
import { fmtBytes, fmtCount, fmtCounted, fmtDuration, fmtLatency, fmtRate } from "../../view/format";
import { throughputScales } from "../../view/scales";
import {
  allServersLabel,
  httpVersionLabel,
  OUTCOME,
  PATH,
  PATH_TIP,
  POPULATION,
  reasonLabel,
  STAGE,
  transportLabel,
} from "../../view/words";
import { Icon } from "../kit/icon";
import { tipGroup } from "../kit/rove";
import { ServerScope } from "../kit/scope";
import { tip } from "../kit/tip";
import { LatencyProfileView, ResultSummary, type LaneInput } from "../measure";
import { accountingNotice } from "../measure/lanes-model";

const TITLE = new Intl.DateTimeFormat(undefined, {
  weekday: "short",
  month: "short",
  day: "numeric",
  year: "numeric",
  hour: "2-digit",
  minute: "2-digit",
});
const PROBES_TIP =
  "Probe accounting\nA timeout: no reply by the probe's deadline\nUnfinished or failed sends are not timeouts";
const PARTIAL_TIP = "\nPartial: Some probe outcomes are unknown. Counts cover known outcomes only.";

/** How a role was carried: its mechanism, and the HTTP version each side saw when they differ. */
function path(role: Role, transport: string, protocol?: string, browserProtocol?: string) {
  const observed = browserProtocol && httpVersionLabel(browserProtocol);
  const server = protocol && httpVersionLabel(protocol);
  return {
    value: transportLabel(transport, role) ?? "Not recorded",
    aside:
      observed && server && observed !== server && protocol !== "negotiated"
        ? `${observed}, endpoint ${server}`
        : observed || server || "",
  };
}
const UNMEASURED = { value: "Not measured", aside: "" };

/** Replies, then what else became of the probes; older records may not know their timeouts. */
function probeCounts(summary: LatencySummary): { shown: string; spoken: string } {
  const { probeCount, timeoutCount, unresolvedCount, sendFailureCount } = summary;
  const known = timeoutCount != null;
  const shown = [
    known ? fmtCounted(probeCount - timeoutCount, "reply", "replies") : `${fmtCount(probeCount)} resolved`,
    timeoutCount > 0 && fmtCounted(timeoutCount, "timeout"),
    unresolvedCount > 0 && `${fmtCount(unresolvedCount)} unresolved`,
    sendFailureCount > 0 && fmtCounted(sendFailureCount, "send failure"),
  ].filter(Boolean);
  const counts = [
    `${fmtCount(probeCount)} resolved`,
    known && `${fmtCount(timeoutCount)} timeouts`,
    unresolvedCount != null && `${fmtCount(unresolvedCount)} unresolved`,
    sendFailureCount != null && `${fmtCount(sendFailureCount)} send failures`,
  ]
    .filter(Boolean)
    .join(" · ");
  return {
    shown: shown.join(", "),
    spoken: summary.accountingComplete === false ? `Known: ${counts}. Additional outcomes unknown.` : counts,
  };
}

export function ResultDetail(props: {
  app: App;
  record: HistoryRecord;
  onClose(): void;
  onDelete(invoker: HTMLElement): void;
  /** The result's views threw after they were built. */
  onBroken(): void;
}): HTMLElement {
  const { app, record } = props;
  const { result } = record;
  const run = result.multiServer;
  const multiple = run.selection.length > 1;
  /** "" shows every server together; each result opens on all of them. */
  const lens = signal("");
  // One tier for the whole result, the one its own results set, as the console read it when it finished.
  const units = computed(() => {
    const base = app.preferences.unitBase();
    const kind = app.preferences.unitKind();
    const maximum = app.preferences.config().visualization.throughputMaxBytesPerSec;
    return { base, kind, tier: throughputScales(result, null, maximum, { base, kind }).tier };
  });
  // Latency follows the chosen server when it measured latency, else the run's latency server.
  const latencyServer = computed(
    () =>
      run.servers.find(({ server, latencyTarget }) => server.id === lens() && latencyTarget) ??
      run.servers.find(({ server }) => server.id === run.latencyFocus),
  );
  const summaries = () => latencyServer()?.latencyByStage ?? result.latencyByStage;
  const lanes = computed(() =>
    STAGES.flatMap((stage): LaneInput[] => {
      const summary = summaries()[stage];
      return summary &&
        (accountingNotice(summary) || [summary.minMs, summary.maxMs, summary.p50Ms].some((ms) => ms != null))
        ? [{ stage, summary }]
        : [];
    }),
  );
  // Only a probe channel accounts for each probe.
  const accounting = computed(() => {
    const transport = latencyServer()?.latencyTarget?.transport;
    if (transport !== "websocket" && transport !== "webtransport") return [];
    return STAGES.flatMap((stage) => {
      const summary = summaries()[stage];
      return summary &&
        (summary.accountingComplete === false ||
          summary.probeCount > 0 ||
          summary.unresolvedCount > 0 ||
          summary.sendFailureCount > 0)
        ? [{ stage, summary }]
        : [];
    });
  });
  // A choice that makes the views throw shows the result as unreadable, as a throw while it is first built does.
  const guard = (update: () => void) => {
    let threw = false;
    const caught = () => {
      threw = true;
    };
    addEventListener("error", caught);
    try {
      update();
    } finally {
      removeEventListener("error", caught);
    }
    if (threw) props.onBroken();
  };
  const transferred = movedBytes(run);
  const title = `result-${record.id}-title`;

  const fact = (label: string, value: Node | string | (() => string), aside = "") => (
    <div>
      <dt>{label}</dt>
      <dd>
        {value}
        {aside && <span class="aside">{aside}</span>}
      </dd>
    </div>
  );
  const pathFact = (role: Role, { value, aside }: { value: string; aside: string }) => (
    <div>
      <dt ref={tip(() => PATH_TIP[role])}>{PATH[role]}</dt>
      <dd>
        {value}
        {aside && <span class="aside">{aside}</span>}
      </dd>
    </div>
  );

  const servers = run.selection.map((server) => {
    const measured = run.servers.find((entry) => entry.server.id === server.id);
    const failure = run.failures.find((entry) => entry.serverId === server.id);
    const ip = measured?.throughput.clientIpVersion;
    return (
      <section class="group">
        <div class="group-head">
          <h3>{multiple ? server.name : "Server"}</h3>
        </div>
        <dl class="kv" ref={tipGroup}>
          {!multiple && fact("Name", server.name)}
          {server.location && fact("Location", server.location)}
          {server.url && fact("Address", server.url)}
          {failure && (
            <div>
              <dt>Status</dt>
              <dd class="status">
                <span class="status-dot inline" data-tone="err" />
                {reasonLabel(failure.reason)}
              </dd>
            </div>
          )}
          {ip && fact("IP family", `IPv${ip}`)}
          {multiple && [
            fact(STAGE.download.short, () => fmtRate(measured?.download?.reportedBytesPerSec, units())),
            fact(STAGE.upload.short, () => fmtRate(measured?.upload?.reportedBytesPerSec, units())),
            fact(STAGE.latency.short, fmtLatency(measured?.latency?.reportedMs)),
          ]}
          {pathFact(
            "throughput",
            measured
              ? path(
                  "throughput",
                  measured.throughput.transport,
                  measured.throughput.protocol,
                  measured.throughput.browserProtocol,
                )
              : UNMEASURED,
          )}
          {pathFact(
            "latency",
            measured?.latencyTarget ? path("latency", measured.latencyTarget.transport) : UNMEASURED,
          )}
        </dl>
      </section>
    );
  });

  return (
    <article class="result-detail" aria-labelledby={title} tabindex="-1">
      <header class="sheet-head detail-head page-fill">
        <button class="btn btn-quiet back" type="button" aria-label="Back to results" onclick={props.onClose}>
          <span aria-hidden="true">←</span>
          Results
        </button>
        <div class="title">
          <h2 id={title}>
            <time datetime={new Date(record.completedAt).toISOString()}>{TITLE.format(record.completedAt)}</time>
          </h2>
          {result.outcome !== "complete" && (
            <span class="outcome">
              <span class="status-dot inline" data-tone={OUTCOME[result.outcome].tone} />
              {OUTCOME[result.outcome].label}
            </span>
          )}
        </div>
        <dl class="head-facts">
          <div>
            <dt>Duration</dt>
            <dd>{fmtDuration(result.durationMs)}</dd>
          </div>
          <div>
            <dt>Transferred</dt>
            <dd>{() => fmtBytes(transferred, app.preferences.unitBase())}</dd>
          </div>
        </dl>
        <div class="head-actions">
          {multiple && (
            <ServerScope
              quiet
              label="Servers shown in this result"
              servers={() => run.selection}
              value={lens}
              onChange={(id) => guard(() => lens.set(id))}
              aggregate={() => allServersLabel(run.selection.length, run.participants.length, run.servers.length)}
              unavailable={() =>
                run.selection
                  .filter(({ id }) => !run.servers.some(({ server }) => server.id === id))
                  .map(({ id }) => id)
              }
            />
          )}
          <button
            class="btn btn-icon btn-quiet"
            type="button"
            aria-label="Delete this result"
            ref={tip(() => "Delete this result")}
            onclick={(event) => props.onDelete(event.currentTarget)}
          >
            <Icon name="trash" />
          </button>
          <button
            class="btn btn-icon btn-quiet close-detail"
            type="button"
            aria-label="Close result"
            onclick={props.onClose}
          >
            <Icon name="close" />
          </button>
        </div>
      </header>
      <div class="detail-body">
        <ResultSummary result={() => result} lens={lens} units={units} showWire={app.preferences.showWireEstimates} />
        <div class="facts">
          <Show when={() => lanes().length > 0}>
            {() => (
              <div class="latency">
                <LatencyProfileView
                  lanes={lanes}
                  variant="compact"
                  label="Saved latency distributions"
                  added={() => latencyServer()?.addedLatency ?? result.addedLatency}
                  head={() =>
                    multiple && latencyServer()
                      ? { kind: "source", text: latencyServer()!.server.name }
                      : { kind: "none" }
                  }
                />
              </div>
            )}
          </Show>
          {servers}
          <Show when={() => accounting().length > 0}>
            {() => (
              <section class="group">
                <h3>
                  <span
                    ref={tip(() =>
                      accounting().some(({ summary }) => summary.accountingComplete === false)
                        ? PROBES_TIP + PARTIAL_TIP
                        : PROBES_TIP,
                    )}
                  >
                    Probes
                  </span>
                </h3>
                <dl class="kv">
                  <For each={accounting} by={({ stage }) => stage}>
                    {(row) => {
                      const counts = computed(() => probeCounts(row().summary));
                      return (
                        <div
                          data-tone={row().stage}
                          aria-label={() => `${POPULATION[row().stage]}: ${counts().spoken}`}
                        >
                          <dt>{POPULATION[row().stage]}</dt>
                          <dd>
                            {() => counts().shown}
                            {() => row().summary.accountingComplete === false && <span class="aside">Partial</span>}
                          </dd>
                        </div>
                      );
                    }}
                  </For>
                </dl>
              </section>
            )}
          </Show>
          <section class="group">
            <h3>Build</h3>
            <dl class="kv">
              {record.build && fact("Client", record.build)}
              {record.engine && fact("Server", record.engine)}
            </dl>
          </section>
        </div>
      </div>
    </article>
  ) as HTMLElement;
}
