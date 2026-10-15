// The keyboard's one map: Escape closes the topmost surface it reaches, and the shortcut keys act where they apply.
// Pure, so the layering is decided in one place; the shell gathers the facts, runs the command, and draws the hints
// and the tips' key names from the same bindings.
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
  /** A History result is open. */
  detail: boolean;
  /** The visible panels, the last opened last. */
  panels: readonly Panel[];
  /** The panel holding focus. */
  focused: Panel | null;
  /** The visible panel flies over the stage. */
  flyout: boolean;
  /** A start is being prepared. */
  pending: boolean;
}

export type Command =
  | { kind: "close"; panel: Panel }
  | { kind: "close-detail" }
  | { kind: "close-history" }
  | { kind: "cancel-start" }
  | { kind: "run" }
  | { kind: "toggle"; panel: Panel }
  | { kind: "history" }
  | { kind: "theme" };

export interface Binding {
  key: string;
  /** The keycap the hints and tips name. */
  cap: string;
  label: string;
  /** Whether the key acts at all; the History key acts only while results are saved. */
  when(facts: Pick<KeyFacts, "saving">): boolean;
  command: Command;
  /** The status strip names it. */
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
  theme: { key: "t", cap: "T", label: "Theme", when: always, command: { kind: "theme" }, hint: false },
} as const satisfies Record<string, Binding>;

/** Space runs the test where no focused control takes it; R does wherever text entry does not. */
export const RUN_CAP = "Space";

/** What a key press does, or null when the page leaves it alone. Escape never stops a running test. */
export function command(facts: KeyFacts): Command | null {
  if (facts.key === "Escape") return dismiss(facts);
  if (facts.editable || !facts.shortcuts) return null;
  const key = facts.key.toLowerCase();
  if (facts.key === " " ? !facts.ownsSpace : key === "r")
    return facts.workspace === "measurement" ? { kind: "run" } : null;
  const binding: Binding | undefined = Object.values(BINDINGS).find((each) => each.key === key);
  return binding?.when(facts) ? binding.command : null;
}

// The panel holding focus, then a flyout even from a text field; text entry keeps every later layer.
function dismiss(facts: KeyFacts): Command | null {
  const last = facts.panels.at(-1);
  if (facts.focused) return { kind: "close", panel: facts.focused };
  if (facts.flyout && last) return { kind: "close", panel: last };
  if (facts.editable) return null;
  if (facts.workspace === "history") return facts.detail ? { kind: "close-detail" } : { kind: "close-history" };
  if (facts.workspace !== "measurement") return null;
  if (last) return { kind: "close", panel: last };
  return facts.pending ? { kind: "cancel-start" } : null;
}
