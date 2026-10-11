// The top bar: start over, the Settings key, the connection, the way back to a live run, the account, History,
// Details and the theme, with the last three moving into "More controls" as the bar narrows (container queries).
import type { App } from "../../app/app";
import type { Theme } from "../../app/preferences";
import { Show } from "../../lib/dom";
import { computed, onCleanup } from "../../lib/signal";
import { PHASE, STAGE } from "../../view/words";
import { AccountControl } from "../account/account";
import { Icon, type IconName } from "../kit/icon";
import { MoreMenu } from "../kit/menu";
import { keyHint, tip } from "../kit/tip";
import type { Shell } from "./api";
import { BINDINGS, type Binding } from "./keys";
import type { Controller } from "./shell";

const THEME: Record<Theme, { label: string; icon: IconName }> = {
  light: { label: "Light", icon: "sun" },
  dark: { label: "Dark", icon: "moon" },
  auto: { label: "Auto", icon: "contrast" },
};

/** Builds what a key opens as the hand or focus reaches it, so the press only reveals it. */
const intent = (build: () => void) => (node: HTMLElement) => {
  const listening = new AbortController();
  const once = () => {
    listening.abort();
    build();
  };
  node.addEventListener("pointerenter", once, listening);
  node.addEventListener("focusin", once, listening);
  onCleanup(() => listening.abort());
};

export function TopBar(props: { app: App; shell: Shell; ctl: Controller; connectivity: Node }): Node {
  const { app, shell, ctl } = props;
  const theme = app.preferences.theme;
  const measuring = computed(() => ctl.workspace() === "measurement");
  // A tip names the key only while the page's shortcuts act on it.
  const named = (binding: Binding, text: () => string) =>
    tip(
      () =>
        `${text()}${keyHint(binding.cap, app.preferences.keyShortcuts() && binding.when({ saving: app.saving() }))}`,
    );
  const buildDetails = intent(() => ctl.visited.details.set(true));

  // While History or a missing page shows, the running test is one press away.
  const live = computed(() => {
    if (measuring()) return null;
    const phase = app.run.phase();
    if (app.run.preparing()) return { icon: "bolt", label: PHASE.connecting, tone: "warmup" } as const;
    if (phase === "connecting" || phase === "warmup")
      return { icon: "bolt", label: PHASE[phase], tone: "warmup" } as const;
    if (!(phase in STAGE)) return null;
    const stage = STAGE[phase as keyof typeof STAGE];
    return { icon: stage.icon, label: `${stage.label}${app.run.stall() ? ", recovering" : ""}`, tone: stage.tone };
  });

  return (
    <header class={() => (app.saving() ? "topbar saving" : "topbar")}>
      <button
        class="brand-btn"
        type="button"
        aria-label={() => `Graphite Meter — ${measuring() ? "start over" : "return to measurement"}`}
        ref={tip(() => (measuring() ? "Start over" : "Return to measurement"))}
        onclick={() => shell.startOver()}
      >
        <svg class="brand-glyph" viewBox="0 0 24 24" aria-hidden="true">
          <path class="brand-hex" d="M12 2.6 3.9 7.3v9.4l8.1 4.7 8.1-4.7V7.3Z" />
          <path class="brand-needle" d="M12 12 18.6 8.2" />
          <circle cx="12" cy="12" r="2.1" fill="currentColor" />
        </svg>
        <span class="brand-label">Graphite{" "}Meter</span>
      </button>
      <button
        class="btn btn-icon key"
        type="button"
        aria-label="Settings"
        aria-expanded={() => String(shell.shown("settings"))}
        ref={(node) => {
          intent(() => ctl.visited.settings.set(true))(node);
          named(BINDINGS.settings, () => "Settings")(node);
        }}
        onclick={(event) => shell.toggle("settings", event.currentTarget)}
      >
        <Icon name="settings" />
      </button>
      <div class="connectivity">{props.connectivity}</div>
      <div class="topbar-spacer" />
      <Show when={live}>
        {(chip) => (
          <button
            class="btn return-live"
            type="button"
            data-tone={() => chip().tone}
            aria-label={() => `Live ${chip().label}. Return to measurement.`}
            ref={tip(() => `${chip().label} — return to measurement`)}
            onclick={() => ctl.returnToLive()}
          >
            <span class="run-icon">{() => <Icon name={chip().icon} />}</span>
            <span class="live-copy">
              <strong>Live</strong>
              <span>{() => chip().label}</span>
            </span>
          </button>
        )}
      </Show>
      {app.page.auth && <AccountControl app={app} />}
      <Show when={app.saving}>
        {() => (
          <button
            class="btn btn-icon key direct-history"
            type="button"
            aria-label="History"
            aria-pressed={() => String(ctl.historyOpen())}
            ref={named(BINDINGS.history, () => "History")}
            onclick={(event) => ctl.toggleHistory(event.currentTarget)}
          >
            <Icon name="history" />
          </button>
        )}
      </Show>
      <button
        class="btn btn-icon key direct-details"
        type="button"
        aria-label="Details"
        aria-expanded={() => String(shell.shown("details"))}
        ref={(node) => {
          buildDetails(node);
          named(BINDINGS.details, () => "Details")(node);
        }}
        onclick={(event) => shell.toggle("details", event.currentTarget)}
      >
        <Icon name="info" />
      </button>
      <button
        class="btn btn-icon key direct-theme"
        type="button"
        aria-label={() => `Theme: ${THEME[theme()].label}`}
        ref={named(BINDINGS.theme, () => `Theme: ${THEME[theme()].label}`)}
        onclick={() => ctl.toggleTheme()}
      >
        {/* A new glyph turns into place as the theme changes. */}
        {() => (
          <span class="theme-glyph">
            <Icon name={THEME[theme()].icon} />
          </span>
        )}
      </button>
      <div class="topbar-more" ref={buildDetails}>
        <MoreMenu
          label="More controls"
          items={[
            {
              title: "History",
              detail: "Saved results",
              icon: "history",
              name: "history",
              hidden: computed(() => !app.saving()),
              current: computed(() => (ctl.historyOpen() ? "page" : false)),
              run: (trigger) => ctl.toggleHistory(trigger),
            },
            {
              title: "Details",
              detail: "Server and connection",
              icon: "info",
              // The old client's name, which the URL also keeps.
              name: "endpoint",
              current: computed(() => (shell.shown("details") ? "true" : false)),
              run: (trigger) => shell.toggle("details", trigger),
            },
            {
              title: computed(() => `Theme: ${THEME[theme()].label}`),
              detail: "Cycles light, dark and auto",
              icon: computed(() => THEME[theme()].icon),
              name: "theme",
              run: (trigger) => {
                trigger.focus({ preventScroll: true });
                ctl.toggleTheme();
              },
            },
          ]}
        />
      </div>
    </header>
  );
}
