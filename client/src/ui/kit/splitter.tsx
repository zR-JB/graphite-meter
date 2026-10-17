// A window splitter sits between a panel and its neighbour. Drag it, or move it with the arrows in their own
// direction (16 px, 48 px with Shift), Home and End for the limits, Enter or a double-click to reset.
import { onCleanup, type Read } from "../../lib/signal";

let ids = 0;

/** Tells where a key takes the panel's width within its limits, "reset" for Enter or Space, or null for other keys. */
export function keyed(
  key: string,
  shift: boolean,
  edge: "left" | "right",
  [value, min, max]: readonly [number, number, number],
): number | "reset" | null {
  if (key === "Enter" || key === " ") return "reset";
  const step = (shift ? 48 : 16) * (edge === "right" ? 1 : -1);
  const next =
    key === "ArrowRight"
      ? value + step
      : key === "ArrowLeft"
        ? value - step
        : key === "Home"
          ? min
          : key === "End"
            ? max
            : null;
  return next === null ? null : Math.min(max, Math.max(min, Math.round(next)));
}

export function Splitter(props: {
  /** The splitter sits on this edge of the panel, and the panel grows towards it. */
  edge: "left" | "right";
  label: string;
  /** The panel's width in px, and its limits. */
  value: Read<number>;
  min: Read<number>;
  max: Read<number>;
  onChange(px: number): void;
  onReset(): void;
}): Node {
  const clamp = (px: number) => Math.min(props.max.peek(), Math.max(props.min.peek(), Math.round(px)));
  const grows = props.edge === "right" ? 1 : -1;
  // A move that lands where the panel already is writes nothing, so a limit never rewrites a preference.
  const change = (px: number) => {
    if (px !== props.value.peek()) props.onChange(px);
  };
  let drag: { start: number; width: number; release: () => void } | null = null;
  onCleanup(() => drag?.release());
  const help = `splitter-${++ids}-help`;
  return (
    <>
      <div
        class="resize-handle"
        data-edge={props.edge}
        role="separator"
        aria-orientation="vertical"
        aria-label={props.label}
        aria-describedby={help}
        aria-valuemin={() => String(props.min())}
        aria-valuemax={() => String(props.max())}
        aria-valuenow={() => String(props.value())}
        aria-valuetext={() => `${props.value()} pixels wide`}
        tabindex="0"
        onpointerdown={(event) => {
          if (!event.isPrimary || event.button !== 0) return;
          drag?.release();
          event.preventDefault();
          const handle = event.currentTarget;
          handle.setPointerCapture(event.pointerId);
          const body = document.body.style;
          const [select, cursor] = [body.userSelect, body.cursor];
          body.userSelect = "none";
          body.cursor = "col-resize";
          const move = (moved: PointerEvent) => {
            if (drag) change(clamp(drag.width + grows * (moved.clientX - drag.start)));
          };
          const release = () => {
            handle.removeEventListener("pointermove", move);
            for (const type of ["pointerup", "pointercancel", "lostpointercapture"])
              handle.removeEventListener(type, release);
            [body.userSelect, body.cursor] = [select, cursor];
            drag = null;
          };
          drag = { start: event.clientX, width: props.value.peek(), release };
          handle.addEventListener("pointermove", move);
          for (const type of ["pointerup", "pointercancel", "lostpointercapture"])
            handle.addEventListener(type, release);
        }}
        onkeydown={(event) => {
          const next = keyed(event.key, event.shiftKey, props.edge, [
            props.value.peek(),
            props.min.peek(),
            props.max.peek(),
          ]);
          if (next === null) return;
          if (next === "reset") props.onReset();
          else change(next);
          event.preventDefault();
        }}
        ondblclick={() => props.onReset()}
      />
      <span hidden id={help}>
        Arrow keys resize, Enter or a double-click resets.
      </span>
    </>
  );
}
