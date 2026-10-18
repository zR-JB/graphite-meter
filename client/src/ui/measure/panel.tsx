// The console body; width classes come from its measured box, as a size container would restyle every frame.
import type { App } from "../../app/app";
import { Show } from "../../lib/dom";
import { computed, effect } from "../../lib/signal";
import { allServersLabel } from "../../view/words";
import { ServerScope } from "../kit/scope";
import type { Shell } from "../shell/api";
import { media } from "../shell/layout";
import { ResultCards } from "./cards";
import { RunButton, StageTrack } from "./chips";
import { LENS } from "./copy";
import { Dial } from "./dial";
import { LatencyProfile } from "./lanes";
import { liveOf, type Live } from "./live";
import { measure } from "./paint";

export function GaugePanel(props: { app: App; shell: Shell }): Node {
  const { app } = props;
  const live = liveOf(app);
  const { run } = app;
  // Under History or a missing page the workspace skips rendering, and its painters wait for its return.
  const away = computed(() => {
    const route = props.shell.route();
    return route.kind !== "app" || route.workspace.kind !== "measurement";
  });
  const section = (
    <section
      class="gauge-panel"
      data-phase={run.phase}
      data-stage={() => (run.running() ? (run.stage() ?? undefined) : undefined)}
    />
  ) as HTMLElement;
  const sizes = measure({ section });
  const width = computed(() => sizes.section().width);
  // A phone on its side sets the dial beside the lanes and controls too; stacked, the ring alone would fill its height.
  const short = media("(max-height: 520px) and (orientation: landscape)");
  const portrait = media("(orientation: portrait)");
  const wide = computed(() => width() >= 760 || (short() && width() >= 560));
  // Beside the lanes the ring centers on their axis with its note just under it; short, the note keeps under the ring.
  const hung = computed(() => wide() && !portrait() && !short());
  effect(() => {
    const list = section.classList;
    list.toggle("wide", wide());
    list.toggle("short", wide() && short());
    list.toggle("compact", width() > 0 && width() <= 520);
    list.toggle("tight", width() > 0 && width() <= 430);
  });

  section.append(
    <div class={() => (live.latencyEnabled() ? "instrument" : "instrument no-lanes")}>
      <Dial app={app} live={live} away={away} hung={hung} lens={<Lens app={app} live={live} />} />
      <Show when={live.latencyEnabled}>
        {() => (
          <div class="latency-panel" data-flip="lanes">
            <LatencyProfile app={app} live={live} away={away} />
          </div>
        )}
      </Show>
      <div class="controls" data-flip="controls">
        <StageTrack app={app} live={live} away={away}>
          <RunButton app={app} shell={props.shell} />
        </StageTrack>
      </div>
      <div class="results">
        <ResultCards app={app} live={live} away={away} />
      </div>
    </div>,
  );
  return section;
}

/** One lens over a multi-server result: every server together, or one; a running stage has only all servers' rate. */
function Lens(props: { app: App; live: Live }): Node {
  const { app, live } = props;
  const servers = live.indicated;
  // A server with no result of its own cannot be shown alone.
  const unmeasured = computed(() => {
    const details = app.run.details();
    return details
      ? servers()
          .filter(({ id }) => !details.servers.some((entry) => entry.server.id === id))
          .map(({ id }) => id)
      : [];
  });
  const stayed = computed(() => {
    const details = app.run.details();
    return details
      ? servers().filter(({ id }) => details.participants.includes(id)).length
      : servers().length;
  });
  return (
    <Show when={() => servers().length > 1}>
      {() => (
        <div class="server-indicator">
          <ServerScope
            quiet
            label={LENS}
            servers={servers}
            value={live.lens}
            onChange={live.showServer}
            aggregate={() =>
              allServersLabel(servers().length, stayed(), servers().length - unmeasured().length)
            }
            unavailable={unmeasured}
            disabled={app.run.running}
          />
        </div>
      )}
    </Show>
  );
}
