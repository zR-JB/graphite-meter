// An overflow menu: a native auto popover gives light dismiss, Escape and the top layer; arrows rove focus, and Tab
// or Escape return to the trigger.
import { signal, type Read } from "../../lib/signal";
import { Icon, type IconName } from "./icon";
import { tip } from "./tip";

export interface MenuItem {
  title: Read<string> | string;
  detail?: string;
  icon: IconName | Read<IconName>;
  /** Names the item for styling and tests (`data-more`). */
  name?: string;
  current?: Read<string | false>;
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
      class="btn btn-icon btn-quiet more-trigger"
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
    [...menu.querySelectorAll<HTMLButtonElement>('[role="menuitem"]:not(:disabled)')].filter((item) =>
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
        const at = list.findIndex((item) => item === document.activeElement);
        const to =
          event.key === "ArrowDown"
            ? (at + 1) % list.length
            : event.key === "ArrowUp"
              ? (at - 1 + list.length) % list.length
              : event.key === "Home"
                ? 0
                : event.key === "End"
                  ? list.length - 1
                  : -1;
        if (to < 0 || !list.length) return;
        event.preventDefault();
        event.stopPropagation();
        focus(list, to);
      }}
    >
      {props.items.map((item) => (
        <button
          type="button"
          role="menuitem"
          tabindex="-1"
          data-more={item.name}
          hidden={item.hidden}
          aria-current={item.current}
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
