// Chooses one server among several, as the lens over results, the server Details inspects or the History detail's.
import { For } from "../../lib/dom";
import { effect } from "../../lib/signal";
import { serverLabel } from "../../view/words";
import { Icon } from "./icon";
import { tip } from "./tip";

interface ScopeServer {
  id: string;
  name: string;
  location?: string;
}

export function ServerScope(props: {
  label: string;
  servers: () => readonly ScopeServer[];
  /** The chosen id, or "" for the aggregate. */
  value: () => string;
  onChange(id: string): void;
  /** Names the choice of every server at once, which `value` reads as "". */
  aggregate?: () => string;
  disabled?: () => boolean;
  /** A server's word after its name, such as its outcome; "" for none. */
  note?: (id: string) => string;
  /** Draws a lens over results, with a server mark and a borderless field. */
  quiet?: boolean;
  /** What the servers ran into, one line each, as the field's tip. */
  issues?: () => string;
}): Node {
  const select = (
    <select
      class={props.quiet ? "server-scope quiet" : "server-scope"}
      aria-label={props.label}
      ref={props.issues && tip(props.issues)}
      disabled={() => props.disabled?.() ?? false}
      onchange={(event) => props.onChange(event.currentTarget.value)}
    >
      {props.aggregate && <option value="">{props.aggregate}</option>}
      <For each={props.servers} by={(server) => server.id}>
        {(server) => (
          <option value={server().id}>
            {() => {
              const note = props.note?.(server().id);
              return note ? `${serverLabel(server())} · ${note}` : serverLabel(server());
            }}
          </option>
        )}
      </For>
    </select>
  ) as HTMLSelectElement;
  // The value is set after the rows' own update, so the chosen option exists.
  effect(() => {
    props.servers();
    select.value = props.value();
  });
  return props.quiet ? (
    <span class="lens">
      <span class="lens-mark">
        <Icon name="server" />
      </span>
      {select}
    </span>
  ) : (
    select
  );
}
