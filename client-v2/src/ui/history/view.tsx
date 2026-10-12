// The list's view options: which value columns show (Date always does) and what the rows are sorted by, in a popover
// under the "Columns" key.
import { naturalDescending, type HistorySort } from "../../app/history/records";
import { HISTORY_COLUMNS, type HistoryColumn } from "../../app/preferences";
import { For } from "../../lib/dom";
import { computed, signal, type Read } from "../../lib/signal";
import { Icon } from "../kit/icon";
import { tip } from "../kit/tip";
import type { Sort } from "./list";
import { orders, SORT_LABEL } from "./model";

// One History shows at a time, so one id serves its anchor.
const ID = "history-view";

export function ViewControl(props: {
  columns: Read<readonly HistoryColumn[]>;
  onColumns(columns: HistoryColumn[]): void;
  sort: Read<Sort>;
  onSort(by: HistorySort, descending: boolean): void;
}): Node {
  const open = signal(false);
  const by = computed(() => props.sort().by);
  const toggle = (column: HistoryColumn) => {
    const shown = props.columns.peek();
    if (!shown.includes(column))
      props.onColumns(HISTORY_COLUMNS.filter((each) => each === column || shown.includes(each)));
    else if (shown.length > 1) props.onColumns(shown.filter((each) => each !== column));
  };
  const check = (on: () => boolean) => <span>{() => on() && <Icon name="check" />}</span>;
  return (
    <div class="view-control">
      <button
        class="btn view-trigger"
        type="button"
        aria-label="Choose columns and sort order"
        aria-haspopup="dialog"
        aria-expanded={() => String(open())}
        popovertarget={ID}
        style={`anchor-name: --${ID}`}
      >
        <span class="layout-icon">
          <Icon name="columns" />
        </span>
        <strong>Columns</strong>
      </button>
      <div
        id={ID}
        class="float popover align-end view-popover"
        popover="auto"
        role="dialog"
        tabindex="-1"
        aria-label="History view options"
        style={`position-anchor: --${ID}`}
        ontoggle={(event) => {
          open.set(event.newState === "open");
          if (event.newState === "open") event.currentTarget.querySelector("button")?.focus();
        }}
      >
        <h3>
          <span ref={tip(() => "Columns\nDate is always shown")}>Columns</span>
        </h3>
        <div class="menu">
          {HISTORY_COLUMNS.map((column) => {
            const on = () => props.columns().includes(column);
            return (
              <button
                type="button"
                role="checkbox"
                aria-checked={() => String(on())}
                disabled={() => on() && props.columns().length === 1}
                onclick={() => toggle(column)}
              >
                {check(on)}
                <span>{SORT_LABEL[column]}</span>
              </button>
            );
          })}
        </div>
        <h3>
          <span ref={tip(() => "Sort by\nResults missing the value stay last")}>Sort by</span>
        </h3>
        <div class="menu" role="radiogroup" aria-label="Sort by">
          <For each={() => ["date" as const, ...props.columns()]} by={(option) => option}>
            {(option) => {
              const chosen = () => by() === option();
              return (
                <button
                  type="button"
                  role="radio"
                  aria-checked={() => String(chosen())}
                  onclick={() => props.onSort(option(), naturalDescending(option()))}
                >
                  {check(chosen)}
                  <span>{SORT_LABEL[option()]}</span>
                </button>
              );
            }}
          </For>
        </div>
        <div class="segmented" role="group" aria-label={() => `Order for ${SORT_LABEL[by()]}`}>
          {() =>
            orders(by()).map(({ descending, label }) => (
              <button
                type="button"
                aria-pressed={() => String(props.sort().descending === descending)}
                onclick={() => props.onSort(by.peek(), descending)}
              >
                {label}
              </button>
            ))
          }
        </div>
      </div>
    </div>
  );
}
