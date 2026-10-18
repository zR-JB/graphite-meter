// A − value + stepper that rolls, takes typed text like a spin button and repeats on a held key.
import { computed, onCleanup } from "../../lib/signal";
import { Roll } from "../kit/roll";
import { stepOnGrid, typedValue, type Direction } from "./model";

const HOLD_MS = 500;
const REPEAT_MS = 150;
const PAGE_STEPS = 10;
const KEYS: Record<string, [Direction, number]> = {
  ArrowUp: [1, 1],
  ArrowDown: [-1, 1],
  PageUp: [1, PAGE_STEPS],
  PageDown: [-1, PAGE_STEPS],
};

export function Stepper(props: {
  /** The field's name, such as "Warmup time", which its keys' names start with. */
  label: () => string;
  value: () => number;
  min: number;
  max: () => number;
  step(value: number, direction: Direction): number;
  format(value: number): string;
  parse(text: string): number | null;
  /** Divides `value` into the spoken unit, so 1000 reads milliseconds as seconds. */
  unit?: number;
  /** The value is a few digits, so the field keeps 3ch and phones offer digits. */
  count?: boolean;
  disabled: () => boolean;
  onChange(value: number): boolean;
}): Node {
  const unit = props.unit ?? 1;
  const text = computed(() => props.format(props.value()));
  // Handlers run outside any effect, so their reads subscribe to nothing.
  const clamp = (value: number) => Math.min(props.max(), Math.max(props.min, value));
  let hold = 0;
  /** A touch that has not yet stepped steps as it lifts, unless the browser took it for a scroll. */
  let tap = false;
  const release = () => {
    clearTimeout(hold);
    hold = 0;
    tap = false;
  };
  onCleanup(release);
  /** Steps `times` times from `from`. A limit holds the value in place. */
  const stepped = (from: number, direction: Direction, times = 1) => {
    let next = from;
    for (let i = 0; i < times; i++)
      next = clamp(stepOnGrid(next, props.step(next, direction), direction));
    return next;
  };
  let field!: HTMLInputElement;
  /** What a step starts from: a typed draft (9 typed over 10 steps up to 10), else the value; text that is no value steps the value. */
  const origin = () => {
    const current = props.value();
    if (field.value === text.peek()) return current;
    const { min, format, parse } = props;
    return typedValue(field.value, current, { min, max: props.max(), format, parse });
  };
  /** Applies `next` and shows what holds, which a typed draft gives way to. Returns whether it applied. */
  const apply = (next: number) => {
    const current = props.value();
    const applied = next === current || props.onChange(next);
    field.value = props.format(applied ? next : current);
    return applied;
  };
  const nudge = (direction: Direction, times = 1) => apply(stepped(origin(), direction, times));
  const atLimit = (direction: Direction) =>
    direction > 0 ? props.value() >= props.max() : props.value() <= props.min;

  const key = (direction: Direction) => (
    <button
      type="button"
      class="btn btn-icon btn-quiet"
      tabindex="-1"
      aria-label={() => `${props.label()}, ${direction > 0 ? "increase" : "decrease"}`}
      aria-disabled={() =>
        String(direction > 0 ? props.value() >= props.max() : props.value() <= props.min)
      }
      disabled={props.disabled}
      onpointerdown={(event) => {
        if (event.button !== 0) return;
        event.currentTarget.setPointerCapture(event.pointerId);
        release();
        tap = event.pointerType === "touch";
        if (!tap && !nudge(direction)) return;
        const repeat = () => {
          tap = false;
          if (atLimit(direction) || !nudge(direction)) return release();
          hold = window.setTimeout(repeat, REPEAT_MS);
        };
        hold = window.setTimeout(repeat, HOLD_MS);
      }}
      onpointerup={() => {
        if (tap) nudge(direction);
        release();
      }}
      onpointercancel={release}
      onlostpointercapture={release}
      oncontextmenu={(event) => event.preventDefault()}
      // A keyboard's or assistive activation arrives as a click without a pointer. A pointer's press already stepped.
      onclick={(event) => {
        if (event.detail === 0) nudge(direction);
      }}
    >
      {direction > 0 ? "+" : "−"}
    </button>
  );

  return (
    <span class={props.count ? "stepper count" : "stepper"}>
      {key(-1)}
      <span class="stepper-field">
        <input
          type="text"
          role="spinbutton"
          autocomplete="off"
          spellcheck="false"
          inputmode={props.count ? "numeric" : undefined}
          value={text}
          disabled={props.disabled}
          ref={(node) => {
            field = node;
          }}
          aria-label={props.label}
          aria-valuemin={props.min / unit}
          aria-valuemax={() => props.max() / unit}
          aria-valuenow={() => props.value() / unit}
          aria-valuetext={text}
          aria-invalid={() => String(props.value() > props.max())}
          onchange={(event) => {
            const input = event.currentTarget;
            const current = props.value();
            const { min, format, parse } = props;
            const next = typedValue(input.value, current, { min, max: props.max(), format, parse });
            input.value = props.format(next === current || props.onChange(next) ? next : current);
          }}
          onkeydown={(event) => {
            const input = event.currentTarget;
            // Escape drops typed text first and keeps the sheet open. The next one closes it.
            if (event.key === "Escape") {
              if (input.value === text.peek()) return;
              input.value = text.peek();
              event.preventDefault();
              return;
            }
            const step = KEYS[event.key];
            // Home and End reach the limits, as a spin button's do.
            const limit =
              event.key === "Home" ? props.min : event.key === "End" ? props.max() : null;
            if (!step && limit === null) return;
            event.preventDefault();
            apply(step ? stepped(origin(), ...step) : limit!);
          }}
        />
        <span class="stepper-shown" aria-hidden="true">
          <Roll text={text} rank={props.value} />
        </span>
      </span>
      {key(1)}
    </span>
  );
}
