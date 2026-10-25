// The console, the page frame every view lives in; Settings, workspaces, status and Details in Tab order.
import "./shell.css";
import type { App } from "../../app/app";
import { markDocument } from "./document";
import { Show } from "../../lib/dom";
import { nextFrame } from "../../lib/frame";
import { insert } from "../../lib/jsx-runtime";
import { computed, effect, onCleanup, scope, type Read } from "../../lib/signal";
import { RUN_ACTION, START_OVER } from "../../view/words";
import { LegalDialog } from "../legal/legal";
import { Announcements } from "../kit/announce";
import { ConfirmDialog } from "../kit/dialog";
import type { Shell } from "./api";
import { BINDINGS, RUN_CAP, STOP_CAP } from "./keys";
import { land } from "./land";
import { createLayout } from "./layout";
import type { Panel } from "./navigation";
import { markScrolled } from "./scrolled";
import { Sheet } from "./sheet";
import { createShell, SIDE, type Controller } from "./shell";
import { TopBar } from "./topbar";

export interface Views {
  measurement(shell: Shell): Node;
  settings(shell: Shell): Node;
  details(shell: Shell): Node;
  /** Built as History opens and disposed as it closes. */
  history(shell: Shell, selectedId: Read<string | null>): Node;
  /** The status strip's facts, before the keyboard hints. */
  status(shell: Shell): Node;
  toast(shell: Shell): Node;
  connectivity(shell: Shell): Node;
}

// Moments that mark a result arriving play when it arrives; what the console mounts with shows settled.
const SETTLES = new Set(["result-sweep", "pop", "grow-x"]);
const finishSettled = (stage: HTMLElement) => {
  for (const animation of stage.getAnimations({ subtree: true }))
    if (animation instanceof CSSAnimation && SETTLES.has(animation.animationName))
      animation.finish();
};

/** Mounts the console into `root` and renders the URL it loaded with; the disposer takes it down again. */
export function mountConsole(
  root: HTMLElement,
  app: App,
  views: Views,
): { shell: Shell; dispose(): void } {
  const [shell, dispose] = scope(() => {
    markDocument(app.preferences);
    const frame = (<main id="console" data-phase={app.run.phase} />) as HTMLElement;
    // In the page first, so the layout starts from the console's real width.
    root.append(frame);
    onCleanup(() => frame.remove());
    const layout = createLayout(frame);
    const ctl = createShell(app, layout);
    const { shell } = ctl;

    // Only the grid's columns change as a dock opens or a handle moves.
    effect(() => {
      const { left, right } = ctl.docks();
      frame.style.gridTemplateColumns = `${left.column}px minmax(0, 1fr) ${right.column}px`;
    });

    const away = computed(() => ctl.workspace() !== "measurement");
    const previous = computed(
      () => app.run.preparation() === "failed" && app.run.phase() !== "idle",
    );
    const measurement = (
      <section
        class={() =>
          `stage measurement-stage${away() ? " away" : ""}${previous() ? " previous" : ""}`
        }
        aria-label="Measurement workspace"
        tabindex="-1"
        data-flip={() => (away() ? undefined : "stage")}
        data-flip-resize=""
        data-stage={() => (app.run.running() ? (app.run.stage() ?? undefined) : undefined)}
        aria-hidden={() => (away() ? "true" : undefined)}
        inert={() => ctl.flyout() || away()}
      >
        {views.measurement(shell)}
      </section>
    ) as HTMLElement;
    ctl.elements.measurement = measurement;
    effect(() => {
      if (away()) land(measurement);
    });

    insert(frame, [
      <TopBar app={app} shell={shell} ctl={ctl} connectivity={views.connectivity(shell)} />,
      sheet(app, ctl, "settings", "Settings", () => views.settings(shell)),
      <Show when={() => ctl.workspace() === "history"}>
        {() => (
          <section
            class="stage history-stage"
            data-flip="stage"
            data-flip-resize=""
            inert={ctl.flyout}
            ref={(node) => {
              ctl.elements.history = node;
              onCleanup(() => {
                if (ctl.elements.history === node) ctl.elements.history = null;
              });
            }}
          >
            {views.history(shell, ctl.selectedId)}
          </section>
        )}
      </Show>,
      <Show when={() => ctl.workspace() === "not-found"}>
        {() => (
          <section
            class="stage history-stage"
            data-flip="stage"
            data-flip-resize=""
            inert={ctl.flyout}
          >
            <div class="empty-state">
              <h1>Page not found</h1>
              <p>That client route does not exist.</p>
              <a class="btn btn-accent" href="#/">
                Return to measurement
              </a>
            </div>
          </section>
        )}
      </Show>,
      measurement,
      <footer class="status" inert={ctl.flyout}>
        {views.status(shell)}
        <ShortcutHints app={app} shell={shell} />
      </footer>,
      sheet(app, ctl, "details", "Details", () => views.details(shell)),
      views.toast(shell),
      <Announcements />,
      <ConfirmDialog
        open={ctl.reset.open}
        id="reset-confirm"
        title={() => START_OVER[ctl.reset.reason()].title}
        description={() => START_OVER[ctl.reset.reason()].description}
        cancel={() => START_OVER[ctl.reset.reason()].cancel}
        confirm={() => START_OVER[ctl.reset.reason()].confirm}
        onCancel={() => ctl.reset.open.set(false)}
        onConfirm={ctl.confirmStartOver}
      />,
      <LegalDialog open={ctl.legalOpen} invoker={ctl.legalInvoker} onClose={ctl.closeLegal} />,
    ]);

    finishSettled(measurement);
    nextFrame(() => finishSettled(measurement));
    ctl.start();
    onCleanup(markScrolled());
    return shell;
  });
  return { shell, dispose };
}

function sheet(app: App, ctl: Controller, panel: Panel, title: string, build: () => Node): Node {
  const side = SIDE[panel];
  return (
    <Sheet
      side={side}
      title={title}
      open={computed(() => ctl.panels().includes(panel))}
      docked={ctl.docked}
      bottomSheet={ctl.shell.layout.bottomSheet}
      dock={computed(() => ctl.docks()[side])}
      preferred={computed(() => app.preferences.dockWidth()[side])}
      built={ctl.visited[panel]}
      onClose={() => ctl.shell.close(panel)}
      onResize={(px) => ctl.resize(side, px)}
      onReset={() => ctl.resetWidth(side)}
      ref={(node) => {
        ctl.elements.sheets[panel] = node;
      }}
    >
      {build}
    </Sheet>
  );
}

/** The keys the page answers, named in the strip while shortcuts are on; the run word's cell fits every run word. */
function ShortcutHints(props: { app: App; shell: Shell }): Node {
  const { app } = props;
  const current = props.shell.runWord;
  // Escape stops a run only with nothing open above the measurement; its cell stays laid out, so the strip never moves.
  const stops = computed(() => {
    const route = props.shell.route();
    return (
      app.run.running() &&
      route.kind === "app" &&
      route.workspace.kind === "measurement" &&
      !route.panels.length &&
      !route.legal
    );
  });
  return (
    <Show when={app.preferences.keyShortcuts}>
      {() => (
        <div class="command-hints" role="group" aria-label="Keyboard shortcuts">
          <span>
            <kbd>{RUN_CAP}</kbd>
            <span class="run-word">
              {Object.values(RUN_ACTION).map((word) => (
                <span aria-hidden={() => (word === current() ? undefined : "true")}>{word}</span>
              ))}
            </span>
          </span>
          {Object.values(BINDINGS)
            .filter((binding) => binding.hint)
            .map((binding) => (
              <Show when={() => binding.when({ saving: app.saving() })}>
                {() => (
                  <span>
                    <kbd>{binding.cap}</kbd>
                    {binding.label}
                  </span>
                )}
              </Show>
            ))}
          <span class="stop-hint" aria-hidden={() => (stops() ? undefined : "true")}>
            <kbd>{STOP_CAP}</kbd>
            {RUN_ACTION.stop}
          </span>
        </div>
      )}
    </Show>
  );
}
