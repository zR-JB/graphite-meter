// A choice of one among a few equal segments; the pressed one carries the thumb.
import { tip } from "../kit/tip";

export interface Segment<T extends string> {
  value: T;
  label: string;
  /** The accessible name, when the word alone is too short. */
  name?: string;
  tip?: () => string;
}

export function Segments<T extends string>(props: {
  label: string;
  options: readonly Segment<T>[];
  value: () => T;
  disabled?: () => boolean;
  choose(value: T): void;
}): Node {
  return (
    <div class="segmented" role="group" aria-label={props.label}>
      {props.options.map((option) => (
        <button
          type="button"
          aria-pressed={() => String(props.value() === option.value)}
          aria-label={option.name}
          disabled={() => props.disabled?.() ?? false}
          ref={option.tip && tip(option.tip)}
          onclick={() => props.choose(option.value)}
        >
          {option.label}
        </button>
      ))}
    </div>
  );
}
