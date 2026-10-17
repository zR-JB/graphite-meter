// A side sheet: one non-modal dialog docked beside the stage, flying over it, or rising from the bottom (shell.css).
// Closed, it is inert and rests once its slide is over, its content kept.
import { Show } from "../../lib/dom";
import type { Child } from "../../lib/jsx-runtime";
import { computed, effect, onCleanup, untrack, type Read } from "../../lib/signal";
import { Icon } from "../kit/icon";
import { Splitter } from "../kit/splitter";
import { dragSheet, type Edge } from "./drag";
import { land } from "./land";
import { clampDock, DOCK, type DockWidths, type Side } from "./layout";

export function Sheet(props: {
  side: Side;
  title: string;
  open: Read<boolean>;
  docked: Read<boolean>;
  bottomSheet: Read<boolean>;
  /** Its column while docked, and how far the handle may take it. */
  dock: Read<DockWidths>;
  /** The width it prefers, before the layout fits it. */
  preferred: Read<number>;
  /** Built on first use, then kept. */
  built: Read<boolean>;
  children: () => Child;
  onClose(): void;
  onResize(px: number): void;
  onReset(): void;
  ref(sheet: HTMLElement): void;
}): Node {
  const { side, title } = props;
  const floating = computed(() => props.open() && !props.docked());
  const sheet = (
    <dialog
      class={`panel sheet side-sheet ${side}`}
      aria-label={title}
      open={props.open}
      inert={() => !props.open()}
      data-rest={untrack(props.open) ? undefined : ""}
      data-flip={() => (props.docked() ? `sheet-${side}` : undefined)}
      data-flip-layout=""
    >
      <div class="sheet-handle" aria-hidden="true">
        <span class="sheet-grip" />
      </div>
      <header class="sheet-head">
        <h2 tabindex="-1">{title}</h2>
        <div class="head-actions">
          <button class="btn btn-icon btn-quiet" type="button" aria-label={`Close ${title}`} onclick={props.onClose}>
            <Icon name="close" />
          </button>
        </div>
      </header>
      <div class="panel-body sheet-body">
        <Show when={props.built}>{props.children}</Show>
      </div>
      {/* After the content, so Tab from the title reaches the controls first. */}
      <Show when={props.docked}>
        {() => (
          <Splitter
            edge={side === "left" ? "right" : "left"}
            label={`Resize ${title} panel`}
            value={computed(() => props.dock().column)}
            min={computed(() => DOCK.min)}
            max={computed(() => props.dock().max)}
            onChange={props.onResize}
            onReset={props.onReset}
          />
        )}
      </Show>
    </dialog>
  ) as HTMLDialogElement;

  // Its own width only, a property its content does not inherit: a resize restyles this one element. A closing
  // docked sheet keeps the width its column gave it, so it leaves as it stood.
  let column = 0;
  effect(() => {
    const flying = clampDock(props.preferred());
    column = props.dock().column || column;
    sheet.style.setProperty("--sheet-w", `${props.docked() ? column || flying : flying}px`);
  });

  // Closed, it rests once whatever moves it has finished, so it leaves in view in every engine.
  effect(() => {
    if (props.open()) return sheet.removeAttribute("data-rest");
    let live = true;
    onCleanup(() => {
      live = false;
    });
    const rest = (): void => {
      if (!live) return;
      const moving = sheet.getAnimations();
      if (moving.length) return void Promise.allSettled(moving.map((animation) => animation.finished)).then(rest);
      land(sheet);
      sheet.setAttribute("data-rest", "");
    };
    // A layout move starts after the route renders, so the moves are gathered once it has.
    queueMicrotask(rest);
  });

  effect(() => {
    if (!floating()) return;
    const edge = (): Edge => (props.bottomSheet.peek() ? "bottom" : side);
    onCleanup(dragSheet(sheet, edge, props.onClose));
  });

  props.ref(sheet);
  return (
    <div class="sheet-layer">
      <button
        class={() => (floating() ? "scrim open" : "scrim")}
        type="button"
        tabindex="-1"
        aria-hidden="true"
        onclick={props.onClose}
      />
      {sheet}
    </div>
  );
}
