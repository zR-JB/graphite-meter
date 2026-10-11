// What the shell offers the views inside it: the route, the layout, and the moves that change what shows.
import type { Read } from "../../lib/signal";
import type { Layout } from "./layout";
import type { Panel, Route } from "./navigation";

export interface Shell {
  readonly route: Read<Route>;
  readonly layout: Layout;
  /** Whether the panel shows now (reactive). */
  shown(panel: Panel): boolean;
  /** `invoker` takes focus back when the surface closes, if it can still take it. */
  toggle(panel: Panel, invoker?: HTMLElement | null): void;
  open(panel: Panel, invoker?: HTMLElement | null): void;
  close(panel: Panel): void;
  /** The History workspace: the list, or one result in it; `null` from an open result steps back to the list. */
  history(id: string | null, invoker?: HTMLElement | null): void;
  /** Back to the measurement; focus returns to whatever opened History. */
  closeHistory(): void;
  legal(invoker?: HTMLElement | null): void;
  /** Start over, asking first when that would stop a running test or drop an unsaved result. */
  startOver(): void;
}
