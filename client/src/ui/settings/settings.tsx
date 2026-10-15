// The Settings sheet's body: the connection, the test's timeline, display, saving, probes and transfers, and a reset.
// A test setting goes through the run, which refuses what can no longer apply; display settings never lock.
import "./settings.css";
import type { App } from "../../app/app";
import { DEFAULT_CONFIG, readPreferences } from "../../app/preferences";
import { BROWSER_POOL, MAX_STREAMS, normalizeStreamCount } from "../../engine/net/budget";
import { parseSelection } from "../../engine/net/paths";
import type { PingCadence, TestConfig } from "../../engine/types";
import { Show } from "../../lib/dom";
import { computed, effect, signal, untrack } from "../../lib/signal";
import { fromRateUnit, rateUnit, toRateUnit } from "../../view/format";
import { CADENCE, READINESS, READINESS_TIP, stageLimitError, TIP } from "../../view/words";
import { announce, announceChanges } from "../kit/announce";
import { ConfirmDialog } from "../kit/dialog";
import { Icon } from "../kit/icon";
import { Switch } from "../kit/switch";
import { tip } from "../kit/tip";
import { liveOf } from "../measure/live";
import type { Shell } from "../shell/api";
import { DurationGroup } from "./duration";
import { parseCount } from "./model";
import { ConnectionPicker } from "./picker";
import { Segments } from "./segments";
import { ServerSelection } from "./servers";
import { flip } from "../flip";
import { Stepper } from "./stepper";
import { Unfold } from "./unfold";

export type Group = "connection" | "duration" | "display" | "probes" | "transfers";
export interface Edits {
  /** Runs a test-setting change; a refusal shows under `group` and is announced, until a change applies. */
  edit(group: Group, change: () => boolean): boolean;
  /** The refusal notice under `group`. */
  notice(group: Group): Node;
}

const REFUSED = "This change cannot apply to the current run.";
/** The maximum keeps one prefix, so a typed number never changes meaning. */
const MAXIMUM_TIER = 2;
/** Requests a browser runs at once per HTTP/1.1 server; more wait for a free connection. */
const CADENCES = Object.keys(CADENCE) as PingCadence[];
const CADENCE_ROWS = [
  ["pingCadence", "Idle latency cadence", TIP.idleCadence],
  ["loadedPingCadence", "Loaded latency cadence", TIP.loadedCadence],
] as const;
const DISPLAY_DEFAULTS = readPreferences(null);

export function SettingsPanel({ app, shell }: { app: App; shell: Shell }): Node {
  const config = app.preferences.config;
  const { servers, run } = app;
  const locked = computed(() => run.running() || run.preparing());
  const custom = signal(false);
  const confirming = signal(false);
  const refused = signal<{ group: Group; text: string } | null>(null);

  const edits: Edits = {
    edit(group, change) {
      // Whatever asks for it, a change that reshapes the console moves it as a chip's does.
      let applied = false;
      flip(() => {
        applied = change();
      });
      if (applied) {
        refused.set(null);
        return true;
      }
      // A live run says why it refused; any other refusal is the change itself.
      const text = run.refusal.peek() || REFUSED;
      refused.set({ group, text });
      announce(text);
      return false;
    },
    notice: (group) => (
      <Show when={() => (refused()?.group === group ? refused()!.text : "")}>
        {(text) => (
          <p class="notice" data-tone="warn">
            {text}
          </p>
        )}
      </Show>
    ),
  };
  const configure = (group: Group, patch: Partial<TestConfig>) => edits.edit(group, () => run.configure(patch));

  // Unselected servers are discovered only while the sheet shows and nothing runs.
  effect(() => {
    const wanted =
      shell.shown("settings") && !!servers.catalogue() && !servers.loading() && !run.running() && !run.preparing();
    untrack(() => servers.showMetadata(wanted));
  });
  // A confirmation belongs to the visible sheet.
  effect(() => {
    if (!shell.shown("settings")) confirming.set(false);
  });

  const limitError = computed(() => stageLimitError(config(), servers.stageLimit()));
  const readiness = computed(() => (app.blocker() ? "blocked" : servers.selectionState()));
  const badge = computed(() => READINESS[readiness()]);
  // Settings stays mounted: only a problem is worth interrupting for.
  announceChanges(() => {
    const { label, tone } = badge();
    return tone === "err" || readiness() === "blocked" ? `Connection: ${label}` : "";
  });

  const { units, scales } = liveOf(app);
  const maximum = computed(() => config().visualization.throughputMaxBytesPerSec);
  const shownMaximum = computed(() => {
    const max = maximum();
    return max === "auto" ? 0 : Number(toRateUnit(max, units(), MAXIMUM_TIER).toPrecision(4));
  });
  const scale = (throughputMaxBytesPerSec: number | "auto") =>
    configure("display", { visualization: { throughputMaxBytesPerSec } });

  const forced = computed(() => config().transferStreams.mode === "forced");
  const streams = (patch: Partial<TestConfig["transferStreams"]>) =>
    configure("transfers", { transferStreams: { ...config.peek().transferStreams, ...patch } });
  const queued = computed(
    () =>
      forced() &&
      config().transferStreams.count > BROWSER_POOL &&
      servers.selected().some((id) => {
        const target = servers.views().get(id)?.throughput.path?.target;
        return target?.transport === "fetch-stream" && target.protocol === "http1";
      }),
  );
  const datagrams = computed(() => {
    const selection = parseSelection(config().transports.throughputTarget);
    return (
      config().experimentalDatagramThroughput ||
      (selection.kind === "transport" && selection.transport === "webtransport-datagram") ||
      (selection.kind === "origin" && selection.mechanism === "::wtdg")
    );
  });

  let resetKey: HTMLButtonElement;
  const reset = () => {
    confirming.set(false);
    if (!run.configure(structuredClone(DEFAULT_CONFIG))) return;
    const { unitBase, unitKind, showWireEstimates, keyShortcuts, explanations } = DISPLAY_DEFAULTS;
    app.preferences.set({
      unitBase,
      unitKind,
      showWireEstimates,
      keyShortcuts,
      explanations,
      resultHistoryPreference: "default",
    });
    custom.set(false);
    refused.set(null);
  };

  return (
    <>
      <div class="settings">
        <section class="group">
          <div class="group-head">
            <h3>Connection</h3>
            <span
              class="badge"
              data-readiness={readiness}
              data-tone={() => badge().tone}
              ref={tip(() => {
                const state = readiness();
                return state === "blocked" ? `${READINESS.blocked.label}\n${app.blocker()}` : READINESS_TIP[state];
              })}
            >
              {() => badge().label}
            </span>
          </div>
          <ServerSelection app={app} locked={locked} />
          <ConnectionPicker app={app} role="throughput" locked={locked} edits={edits} />
          <ConnectionPicker app={app} role="latency" locked={locked} edits={edits} />
          {edits.notice("connection")}
        </section>

        <DurationGroup app={app} edits={edits} custom={custom} limitError={limitError} />

        <section class="group">
          <h3>Display</h3>
          <div class="kv">
            <div class="units">
              <span ref={tip(() => TIP.units)}>Units</span>
              <Segments
                label="Rate unit"
                options={[
                  { value: "bits", label: "Bits" },
                  { value: "bytes", label: "Bytes" },
                ]}
                value={app.preferences.unitKind}
                choose={(unitKind) => app.preferences.set({ unitKind })}
              />
              <Segments
                label="Prefix scale"
                options={[
                  { value: "base10", label: "Decimal" },
                  { value: "base2", label: "Binary" },
                ]}
                value={app.preferences.unitBase}
                choose={(unitBase) => app.preferences.set({ unitBase })}
              />
            </div>
            <div class="units">
              <span ref={tip(() => TIP.explanations)}>Explanations</span>
              <Segments
                label="Explanations"
                options={[
                  { value: "hover", label: "On hover" },
                  { value: "click", label: "On click" },
                ]}
                value={app.preferences.explanations}
                choose={(explanations) => app.preferences.set({ explanations })}
              />
            </div>
            <Switch
              label="Show estimated wire rate"
              tip={() => TIP.wireRate}
              checked={app.preferences.showWireEstimates}
              onToggle={(showWireEstimates) => app.preferences.set({ showWireEstimates })}
            />
            <Switch
              label="Keyboard shortcuts"
              tip={() => TIP.keyShortcuts}
              checked={app.preferences.keyShortcuts}
              onToggle={(keyShortcuts) => app.preferences.set({ keyShortcuts })}
            />
            <Switch
              label="Scale throughput automatically"
              tip={() => TIP.gaugeAuto}
              checked={() => maximum() === "auto"}
              // Fixing the scale keeps the chart's ceiling as it stands.
              onToggle={(auto) => scale(auto ? "auto" : Math.max(1, Math.round(scales.peek().chartBytesPerSec)))}
            />
            <Unfold when={() => maximum() !== "auto"}>
              {() => (
                <label>
                  <span ref={tip(() => TIP.gaugeMax)}>Maximum</span>
                  <span class="field-unit">
                    <input
                      type="number"
                      min="1"
                      value={() => String(shownMaximum())}
                      onchange={(event) => {
                        const input = event.currentTarget;
                        const shown = shownMaximum.peek();
                        const typed = input.valueAsNumber;
                        const value = Number.isFinite(typed) && typed > 0 ? typed : shown;
                        const bytes = Math.max(1, Math.round(fromRateUnit(value, units.peek(), MAXIMUM_TIER)));
                        input.value = String(value === shown || scale(bytes) ? value : shown);
                      }}
                      onkeydown={(event) => {
                        // Escape drops a typed number first and keeps the sheet open; the next one closes it.
                        const input = event.currentTarget;
                        if (event.key !== "Escape" || input.value === String(shownMaximum.peek())) return;
                        input.value = String(shownMaximum.peek());
                        event.preventDefault();
                      }}
                    />
                    <span class="unit">{() => rateUnit(units(), MAXIMUM_TIER)}</span>
                  </span>
                </label>
              )}
            </Unfold>
          </div>
          {edits.notice("display")}
        </section>

        <section class="group">
          <h3>History</h3>
          <div class="kv">
            <Switch
              label="Save results in this browser"
              tip={() => TIP.saveResults}
              checked={app.saving}
              onToggle={(on) => app.preferences.set({ resultHistoryPreference: on ? "enabled" : "disabled" })}
            />
            <a
              class="link-row"
              href="#/history"
              onclick={(event) => {
                event.preventDefault();
                shell.history(null, event.currentTarget);
              }}
            >
              Open History
              <Icon name="chevron" />
            </a>
          </div>
        </section>

        <section class="group">
          <h3>Latency probes</h3>
          <div class="kv">
            {CADENCE_ROWS.map(([key, label, explained]) => (
              <div class="cadence stacked">
                <span ref={tip(() => explained)}>{label}</span>
                <Segments
                  label={label}
                  options={CADENCES.map((value) => ({
                    value,
                    label: CADENCE[value].short,
                    name: CADENCE[value].label,
                  }))}
                  value={() => config()[key]}
                  disabled={locked}
                  choose={(value) =>
                    configure("probes", key === "pingCadence" ? { pingCadence: value } : { loadedPingCadence: value })
                  }
                />
              </div>
            ))}
            <Switch
              label="Skip loaded latency if the Latency stage is off"
              tip={() => TIP.skipLoadedLatency}
              checked={() => config().skipLoadedLatencyWhenStageOff}
              disabled={locked}
              onToggle={(skipLoadedLatencyWhenStageOff) => configure("probes", { skipLoadedLatencyWhenStageOff })}
            />
          </div>
          {edits.notice("probes")}
        </section>

        <section class="group">
          <h3>Transfers</h3>
          <div class="kv">
            <Switch
              label="Force exact stream count"
              tip={() => TIP.forcedStreams}
              checked={forced}
              disabled={locked}
              onToggle={(on) => streams({ mode: on ? "forced" : "auto" })}
            />
            <div class="units">
              <span ref={tip(() => (forced() ? TIP.forcedStreamCount : TIP.autoStreamCount))}>
                {() => (forced() ? "Streams per server and direction" : "HTTP/1.1 stream limit per direction")}
              </span>
              <Stepper
                label={() => (forced() ? "streams" : "stream limit")}
                verbs={["Fewer", "More"]}
                value={() => config().transferStreams.count}
                min={1}
                max={() => MAX_STREAMS}
                step={() => 1}
                format={String}
                parse={parseCount}
                count
                disabled={locked}
                onChange={(count) => streams({ count: normalizeStreamCount(count) })}
              />
            </div>
            <Switch
              label="Datagram throughput (experimental)"
              tip={() => TIP.datagramThroughput}
              checked={() => config().experimentalDatagramThroughput}
              disabled={locked}
              onToggle={(experimentalDatagramThroughput) => configure("transfers", { experimentalDatagramThroughput })}
            />
          </div>
          {edits.notice("transfers")}
          <Show when={queued}>
            {() => (
              <p class="hint">
                Browsers run {BROWSER_POOL} HTTP/1.1 requests per server at once. Streams past that wait for a free
                connection.
              </p>
            )}
          </Show>
          <Show when={servers.streamPlanError}>
            {(text) => (
              <p class="notice" data-tone="warn" role="status">
                {text}
              </p>
            )}
          </Show>
          <Show when={datagrams}>
            {() => (
              <p class="notice" data-tone="warn" role="status">
                <span>
                  <strong>Datagram delivery, not packet loss.</strong> Datagrams are never resent; expect lower rates
                  than streams, mostly for uploads.
                </span>
              </p>
            )}
          </Show>
        </section>

        <button
          class="btn btn-danger reset"
          type="button"
          disabled={locked}
          ref={(button) => (resetKey = button)}
          onclick={() => confirming.set(true)}
        >
          Reset settings
        </button>
      </div>
      <ConfirmDialog
        open={confirming}
        id="settings-reset-confirm"
        title="Reset settings?"
        description="Test, display and history-saving settings return to their defaults. Your theme, panel layout and saved results are kept."
        cancel="Keep settings"
        confirm="Reset settings"
        invoker={() => resetKey}
        onCancel={() => confirming.set(false)}
        onConfirm={reset}
      />
    </>
  );
}
