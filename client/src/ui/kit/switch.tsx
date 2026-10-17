// The label names a switch row and an optional tip describes it. The owner decides whether the change applies.
import { tip } from "./tip";

let ids = 0;

export function Switch(props: {
  checked: () => boolean;
  label: string;
  disabled?: () => boolean;
  tip?: () => string;
  onToggle(next: boolean): void;
}): Node {
  const described = props.tip ? `switch-${++ids}-tip` : undefined;
  return (
    <label class={() => (props.disabled?.() ? "switch link-row disabled" : "switch link-row")}>
      <input
        class="sr-only"
        type="checkbox"
        role="switch"
        aria-describedby={described}
        checked={props.checked}
        disabled={() => props.disabled?.() ?? false}
        onchange={(event) => {
          const next = event.currentTarget.checked;
          // The row is controlled, so it shows what the owner keeps.
          event.currentTarget.checked = props.checked();
          props.onToggle(next);
        }}
      />
      <span class="switch-track" aria-hidden="true">
        <span class="switch-knob" />
      </span>
      {described && (
        <span hidden id={described}>
          {props.tip}
        </span>
      )}
      <span class="switch-label" ref={props.tip ? tip(props.tip) : undefined}>
        {props.label}
      </span>
    </label>
  );
}
