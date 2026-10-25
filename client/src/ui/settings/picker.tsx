// A path picker: the role's choices on the selected servers, unavailable ones folded, and the role's check.
import type { App } from "../../app/app";
import type { Role } from "../../engine/types";
import { For, Show } from "../../lib/dom";
import { computed, signal, type Read } from "../../lib/signal";
import { PATH, PATH_NOTE, PATH_TIP, READINESS } from "../../view/words";
import { tip } from "../kit/tip";
import { TRANSPORT_KEY } from "../../engine/net/servers";
import { choices, roleLine, type Choice } from "./paths";
import type { Edits } from "./settings";
import { Unfold } from "./unfold";

export function ConnectionPicker(props: {
  app: App;
  role: Role;
  locked: Read<boolean>;
  edits: Edits;
}): Node {
  const { app, role, locked } = props;
  const { servers } = app;
  const config = app.preferences.config;
  const labelId = `settings-${role}-path`;
  const selected = computed(() => config().transports[TRANSPORT_KEY[role]]);
  const options = computed(() => {
    const ids = servers.selected();
    const views = servers.views();
    const chosen = (servers.catalog()?.servers ?? []).filter((entry) => ids.includes(entry.id));
    return choices(
      role,
      chosen.map((entry) => views.get(entry.id)?.server ?? entry),
      views,
      config(),
      app.page,
    );
  });
  // Unavailable choices fold into one line after the available ones. The chosen one always shows.
  const unfolded = signal(false);
  const folded = computed(() =>
    options().filter((option) => option.unavailable !== null && option.value !== selected()),
  );
  const listed = computed(() => options().filter((option) => !folded().includes(option)));
  let picker: HTMLDivElement;

  const choose = (value: string) =>
    props.edits.edit("connection", () =>
      app.run.configure({
        transports: { ...config.peek().transports, [TRANSPORT_KEY[role]]: value },
      }),
    );
  const row = (option: Read<Choice>) => (
    <label
      class={() =>
        option().unavailable !== null || (locked() && option().value !== selected())
          ? "unavailable"
          : false
      }
    >
      <input
        class="check"
        type="radio"
        name={`${role}-target`}
        value={option.peek().value}
        checked={() => selected() === option().value}
        disabled={() => option().unavailable !== null || locked()}
        onchange={(event) => {
          if (choose(event.currentTarget.value)) return;
          for (const radio of picker.querySelectorAll<HTMLInputElement>('input[type="radio"]'))
            radio.checked = radio.value === selected.peek();
        }}
      />
      <span class="choice-label">
        {() => option().label}
        <Show when={() => option().unavailable ?? PATH_NOTE[role][option().group]}>
          {(note) => <small>{note}</small>}
        </Show>
      </span>
    </label>
  );

  // The servers a failure or sign-in is listed for above are left out. The paths speak for the rest.
  const considered = computed(() => {
    const ids = servers.selected();
    const views = servers.views();
    return ids.flatMap((id) => {
      const view = views.get(id);
      return view &&
        view.readiness !== "sign-in" &&
        !(ids.length > 1 && view.readiness === "failed")
        ? [view]
        : [];
    });
  });
  const line = computed(() => {
    const shown = roleLine(role, considered(), servers.selected().length > 1, app.page);
    return shown && locked() ? { ...shown, status: READINESS["in-use"] } : shown;
  });
  const offerAutomatic = computed(
    () =>
      selected() !== "auto" &&
      !locked() &&
      (line()?.state === "failed" ||
        options().some((option) => option.value === selected() && option.unavailable !== null) ||
        servers.selected().some((id) => servers.views().get(id)?.readiness === "failed")),
  );
  const offerRetry = computed(() => {
    const state = line()?.state;
    return !locked() && (state === "failed" || state === "unchecked");
  });

  return (
    <div
      class="picker"
      role="group"
      aria-labelledby={labelId}
      ref={(element) => (picker = element)}
    >
      <div class="list-label">
        <span id={labelId} ref={tip(() => PATH_TIP[role])}>
          {PATH[role]}
        </span>
      </div>
      <div class="kv choices">
        {/* The list follows the servers and their checks at once. Only rows a person unfolds unfold. */}
        <For each={listed} by={(option) => option.value}>
          {row}
        </For>
        <Unfold when={unfolded}>
          {() => (
            <For each={folded} by={(option) => option.value}>
              {row}
            </For>
          )}
        </Unfold>
        <Show when={() => folded().length > 0}>
          {() => (
            <button
              class="fold"
              type="button"
              aria-expanded={() => String(unfolded())}
              ref={tip(() =>
                [
                  "Unavailable paths",
                  ...folded().map((option) => `${option.label}: ${option.unavailable}`),
                ].join("\n"),
              )}
              onclick={() => unfolded.set(!unfolded.peek())}
            >
              {() => (unfolded() ? "Hide unavailable" : `${folded().length} more unavailable`)}
            </button>
          )}
        </Show>
      </div>
      {/* The line keeps its slot from the start, though until the server list loads there is nothing to check or retry. */}
      <div class="validation">
        <Show when={() => (servers.catalog() ? line() : null)}>
          {(shown) => (
            <p>
              <span class="status-dot inline" data-tone={() => shown().status.tone} />
              <strong>{() => shown().status.label}</strong>
              {() => (shown().summary ? ` ${shown().summary}` : "")}
            </p>
          )}
        </Show>
        <Show when={() => !!servers.catalog() && offerAutomatic()}>
          {() => (
            <button class="btn" type="button" onclick={() => choose("auto")}>
              Use Automatic
            </button>
          )}
        </Show>
        <Show when={() => !!servers.catalog() && offerRetry()}>
          {() => (
            // Both pickers show at once, and a group's name does not name its buttons, hence "Retry Latency path".
            <button
              class="btn"
              type="button"
              aria-label={`Retry ${PATH[role]}`}
              onclick={() => void servers.retry(undefined, role)}
            >
              Retry
            </button>
          )}
        </Show>
      </div>
    </div>
  );
}
