// The shell offers the views inside it the route, the layout, and the moves that change what shows.
import type { Read } from "../../lib/signal";
import type { Layout } from "./layout";
import type { Panel, Route } from "./navigation";

export interface Shell {
  readonly route: Read<Route>;
  readonly layout: Layout;
  /** Tells whether the panel shows now (reactive). */
  shown(panel: Panel): boolean;
  /** `invoker` takes focus back when the surface closes, if it can still take it. */
  toggle(panel: Panel, invoker?: HTMLElement | null): void;
  close(panel: Panel): void;
  /** Opens the History workspace, the list or one result in it. `null` from an open result steps back to the list. */
  history(id: string | null, invoker?: HTMLElement | null): void;
  /** Shows another result in the open one's place, on the same history entry. */
  swapResult(id: string): void;
  /** Returns to the measurement. Focus returns to whatever opened History. */
  closeHistory(): void;
  legal(invoker?: HTMLElement | null): void;
  /** Starts over, asking first when that would stop a running test or drop an unsaved result. */
  startOver(): void;
  /** Starts or stops a test, asking first before a new one replaces a result whose save failed. */
  toggleRun(): void;
}
