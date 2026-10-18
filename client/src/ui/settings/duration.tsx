// The Duration group: the planned length, presets, per-stage steppers and the timeline switches.
import { DURATION_LIMITS, DURATION_PRESETS } from "../../app/preferences";
import type { App } from "../../app/app";
import type { Stage } from "../../engine/run/schedule";
import type { TestConfig } from "../../engine/types";
import { For, Show } from "../../lib/dom";
import { computed, effect, type Signal } from "../../lib/signal";
import { fmtDuration, fmtStageTime, parseDuration } from "../../view/format";
import { PHASE, STAGE, TIP } from "../../view/words";
import { announceChanges } from "../kit/announce";
import { Roll } from "../kit/roll";
import { Switch } from "../kit/switch";
import { tip } from "../kit/tip";
import { PRESETS, presetOf, stageStepMs, type Preset } from "./model";
import { Segments } from "./segments";
import type { Edits } from "./settings";
import { Stepper } from "./stepper";
import { Unfold } from "./unfold";

type DurationKey = keyof TestConfig["duration"];
const WARMUP_STEP_MS = 100;
const CUSTOM_STAGES = ["latency", "download", "upload"] as const;
// Latency, Download and Upload always show; Bidirectional with its switch.
const STRIP: readonly Stage[] = CUSTOM_STAGES;
const STRIP_BIDIRECTIONAL: readonly Stage[] = [...CUSTOM_STAGES, "bidirectional"];
const NAME = { short: "Short", medium: "Medium", long: "Long", custom: "Custom" } as const;

export function DurationGroup(props: { app: App; edits: Edits; custom: Signal<boolean> }) {
  const { app, edits, custom } = props;
  const config = app.preferences.config;
  const mode = computed((): Preset | "custom" => (custom() ? "custom" : (presetOf(config().duration) ?? "custom")));
  const ms = (key: DurationKey) => () => config().duration[key];

  const setDuration = (key: DurationKey, value: number) =>
    value === config.peek().duration[key] ||
    edits.edit("duration", () => app.run.configure({ duration: { ...config.peek().duration, [key]: value } }));
  const choose = (preset: Preset | "custom") => {
    custom.set(preset === "custom");
    if (preset !== "custom")
      edits.edit("duration", () => app.run.configure({ duration: { ...DURATION_PRESETS[preset] } }));
  };
  const presetTip = (preset: Preset | "custom") => {
    if (preset === "custom") return `${NAME.custom}\nSet each stage's time`;
    const times = DURATION_PRESETS[preset];
    const stages = config().stages.bidirectional ? STRIP_BIDIRECTIONAL : STRIP;
    const parts = [`${PHASE.warmup} ${fmtStageTime(times.warmupMs)}`];
    for (const stage of stages) parts.push(`${STAGE[stage].label} ${fmtStageTime(times[`${stage}Ms`])}`);
    return `${NAME[preset]}\n${parts.join(" · ")}`;
  };

  const stepper = (key: DurationKey, label: string) => {
    const [min, max] = DURATION_LIMITS[key];
    return (
      <Stepper
        label={() => label}
        value={ms(key)}
        min={min}
        // A stage stays within what every selected server admits.
        max={key === "warmupMs" ? () => max : () => Math.min(max, app.servers.stageLimit().ms)}
        step={key === "warmupMs" ? () => WARMUP_STEP_MS : stageStepMs}
        format={fmtStageTime}
        parse={parseDuration}
        unit={1000}
        disabled={app.run.preparing}
        onChange={(value) => setDuration(key, value)}
      />
    );
  };
  const stageRow = (stage: Stage) => (
    <div class="stage-row">
      <span class="stage-name" data-tone={stage}>
        {STAGE[stage].label}
      </span>
      {stepper(`${stage}Ms`, `${STAGE[stage].label} stage time`)}
    </div>
  );
  const warmup = ms("warmupMs");
  announceChanges(props.app.limitError);
  const strip = computed(() => (config().stages.bidirectional ? STRIP_BIDIRECTIONAL : STRIP));

  return (
    <section class="group">
      <div class="group-head">
        <h3>
          <span ref={tip(() => TIP.stageTime)}>Duration</span>
        </h3>
        <span class="aside">
          <Roll text={() => fmtDuration(app.plannedMs(), 0)} rank={app.plannedMs} /> in total
        </span>
      </div>
      <div class="kv">
        <div class="presets">
          <Segments
            label="Duration preset"
            options={[...PRESETS, "custom" as const].map((preset) => ({
              value: preset,
              label: NAME[preset],
              tip: () => presetTip(preset),
            }))}
            value={mode}
            disabled={app.run.preparing}
            choose={choose}
          />
        </div>
        <div class="strip-row">
          <div class="duration-strip" role="list" aria-label="Stage times">
            <For each={strip} by={(stage) => stage}>
              {(stage) => {
                const time = ms(`${stage.peek()}Ms`);
                return (
                  <div
                    class="strip-segment"
                    role="listitem"
                    data-tone={stage.peek()}
                    ref={(segment) => effect(() => segment.style.setProperty("--ms", String(time())))}
                  >
                    <span class="strip-bar" aria-hidden="true" />
                    <span class="strip-time">
                      <Roll text={() => fmtStageTime(time())} rank={time} />
                    </span>
                    <span class="strip-name">{STAGE[stage.peek()].short}</span>
                  </div>
                );
              }}
            </For>
          </div>
        </div>
        <Unfold when={() => mode() === "custom"}>
          {() => [
            CUSTOM_STAGES.map(stageRow),
            <Unfold when={() => config().stages.bidirectional}>{() => stageRow("bidirectional")}</Unfold>,
          ]}
        </Unfold>
        <div class="stage-row">
          <span class="stage-name" data-tone="warmup" ref={tip(() => TIP.warmup)}>
            {PHASE.warmup}
          </span>
          <Show
            when={() => mode() === "custom"}
            fallback={() => <Roll text={() => fmtStageTime(warmup())} rank={warmup} />}
          >
            {() => stepper("warmupMs", `${PHASE.warmup} time`)}
          </Show>
        </div>
        <Switch
          label="Bidirectional stage"
          tip={() => TIP.bidirectionalStage}
          checked={() => config().stages.bidirectional}
          disabled={() => !app.run.canToggle("bidirectional")}
          onToggle={(on) =>
            on !== config.peek().stages.bidirectional &&
            edits.edit("duration", () => app.run.toggleStage("bidirectional"))
          }
        />
        <Switch
          label="Finish stable stages early"
          tip={() => TIP.earlyFinish}
          checked={() => config().adaptive}
          disabled={app.run.preparing}
          onToggle={(adaptive) => edits.edit("duration", () => app.run.configure({ adaptive }))}
        />
      </div>
      <Show when={props.app.limitError}>
        {(text) => (
          <p class="notice" data-tone="warn">
            {text}
          </p>
        )}
      </Show>
      {edits.notice("duration")}
      <Show when={app.run.running}>{() => <p class="hint">Changes apply to the current and upcoming stages.</p>}</Show>
    </section>
  );
}
