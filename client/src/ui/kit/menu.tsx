// An overflow menu on a native auto popover; arrows rove focus, Tab or Escape return to the trigger.
import { signal, type Read } from "../../lib/signal";
import { Icon, type IconName } from "./icon";
import { roveTo } from "./rove";
import { tip } from "./tip";

const VERTICAL: Record<string, number> = { ArrowDown: 1, ArrowUp: -1 };

interface MenuItem {
  title: Read<string> | string;
  detail?: string;
  icon: IconName | Read<IconName>;
  /** Names the item for styling and tests (`data-more`). */
  name?: string;
  /** Whether the surface the item toggles is open: the item is a checkbox. */
  checked?: Read<boolean>;
  hidden?: Read<boolean>;
  run(invoker: HTMLButtonElement): void;
}

let ids = 0;

export function MoreMenu(props: { label: string; items: MenuItem[]; danger?: boolean }): Node {
  const id = `menu-${++ids}`;
  const open = signal(false);
  let last = false;
  const trigger = (
    <button
      class="btn btn-icon btn-quiet"
      type="button"
      aria-label={props.label}
      aria-haspopup="menu"
      aria-expanded={() => String(open())}
      aria-controls={id}
      popovertarget={id}
      style={`anchor-name: --${id}`}
      ref={tip(() => props.label)}
      onkeydown={(event) => {
        if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
        event.preventDefault();
        event.stopPropagation();
        last = event.key === "ArrowUp";
        menu.showPopover();
      }}
    >
      <Icon name="more" />
    </button>
  ) as HTMLButtonElement;
  const items = () =>
    [...menu.querySelectorAll<HTMLButtonElement>('[role^="menuitem"]:not(:disabled)')].filter((item) =>
      item.checkVisibility(),
    );
  const focus = (list: HTMLButtonElement[], index: number) => {
    for (const [position, item] of list.entries()) item.tabIndex = position === index ? 0 : -1;
    list[index]?.focus({ preventScroll: true });
  };
  const menu = (
    <div
      id={id}
      class="float popover align-end menu more-menu"
      popover="auto"
      role="menu"
      tabindex="-1"
      aria-label={props.label}
      style={`position-anchor: --${id}`}
      ontoggle={(event) => {
        open.set(event.newState === "open");
        if (event.newState === "open") {
          const list = items();
          focus(list, last ? list.length - 1 : 0);
        }
        last = false;
      }}
      onkeydown={(event) => {
        if (event.key === "Escape" || event.key === "Tab") {
          event.stopPropagation();
          // Native Tab then leaves from the trigger, in either direction.
          if (event.key === "Escape") event.preventDefault();
          trigger.focus({ preventScroll: true });
          menu.hidePopover();
          return;
        }
        const list = items();
        const at = list.indexOf(document.activeElement as HTMLButtonElement);
        const to = roveTo(event.key, at, list.length, VERTICAL);
        if (to < 0 || !list.length) return;
        event.preventDefault();
        event.stopPropagation();
        focus(list, to);
      }}
    >
      {props.items.map((item) => (
        <button
          type="button"
          role={item.checked ? "menuitemcheckbox" : "menuitem"}
          tabindex="-1"
          data-more={item.name}
          hidden={item.hidden}
          aria-checked={item.checked && (() => String(item.checked!()))}
          onclick={() => {
            menu.hidePopover();
            item.run(trigger);
          }}
        >
          <span>{() => <Icon name={typeof item.icon === "string" ? item.icon : item.icon()} />}</span>
          <span>
            <strong>{item.title}</strong>
            {item.detail && <small>{item.detail}</small>}
          </span>
        </button>
      ))}
    </div>
  ) as HTMLDivElement;
  return (
    <div class={props.danger ? "more-control danger" : "more-control"}>
      {trigger}
      {menu}
    </div>
  );
}
