// Maps the keyboard once. Escape closes the topmost surface it reaches, and the shortcut keys act where they apply.
import type { Panel } from "./navigation";

export interface KeyFacts {
  key: string;
  /** Focus is in text entry, which keeps its keys. */
  editable: boolean;
  /** The focused control acts on Space itself. */
  ownsSpace: boolean;
  shortcuts: boolean;
  saving: boolean;
  workspace: "measurement" | "history" | "not-found";
  detail: boolean;
  /** Lists the visible panels, the last opened last. */
  panels: readonly Panel[];
  focused: Panel | null;
  flyout: boolean;
  pending: boolean;
  running: boolean;
}

export type Command =
  | { kind: "close"; panel: Panel }
  | { kind: "close-detail" }
  | { kind: "close-history" }
  | { kind: "cancel-start" }
  | { kind: "stop" }
  | { kind: "run" }
  | { kind: "toggle"; panel: Panel }
  | { kind: "history" }
  | { kind: "theme" };

export interface Binding {
  key: string;
  /** The hints and tips name this keycap. */
  cap: string;
  label: string;
  /** Tells whether the key acts at all. The History key acts only while results are saved. */
  when(facts: Pick<KeyFacts, "saving">): boolean;
  command: Command;
  hint: boolean;
}

const always = () => true;
export const BINDINGS = {
  settings: {
    key: "s",
    cap: "S",
    label: "Settings",
    when: always,
    command: { kind: "toggle", panel: "settings" },
    hint: true,
  },
  details: {
    key: "d",
    cap: "D",
    label: "Details",
    when: always,
    command: { kind: "toggle", panel: "details" },
    hint: true,
  },
  history: {
    key: "h",
    cap: "H",
    label: "History",
    when: (facts) => facts.saving,
    command: { kind: "history" },
    hint: true,
  },
  theme: {
    key: "t",
    cap: "T",
    label: "Theme",
    when: always,
    command: { kind: "theme" },
    hint: false,
  },
} as const satisfies Record<string, Binding>;

/** Space runs the test where no focused control takes it. R does wherever text entry does not. */
export const RUN_CAP = "Space";
/** Escape stops a running test once nothing above the measurement is left to close. */
export const STOP_CAP = "Esc";

/** Decides what a key press does, or null when the page leaves it alone. */
export function command(facts: KeyFacts): Command | null {
  if (facts.key === "Escape") return dismiss(facts);
  if (facts.editable || !facts.shortcuts) return null;
  const key = facts.key.toLowerCase();
  if (facts.key === " " ? !facts.ownsSpace : key === "r")
    return facts.workspace === "measurement" ? { kind: "run" } : null;
  const binding: Binding | undefined = Object.values(BINDINGS).find((each) => each.key === key);
  return binding?.when(facts) ? binding.command : null;
}

// Escape closes the panel holding focus, then a flyout even from a text field. Text entry keeps every later layer.
function dismiss(facts: KeyFacts): Command | null {
  const last = facts.panels.at(-1);
  if (facts.focused) return { kind: "close", panel: facts.focused };
  if (facts.flyout && last) return { kind: "close", panel: last };
  if (facts.editable) return null;
  if (facts.workspace === "history")
    return facts.detail ? { kind: "close-detail" } : { kind: "close-history" };
  if (facts.workspace !== "measurement") return null;
  if (last) return { kind: "close", panel: last };
  if (facts.pending) return { kind: "cancel-start" };
  return facts.running && facts.shortcuts ? { kind: "stop" } : null;
}
