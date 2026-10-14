// A choice of one server among several: the lens over results, the server Details inspects, the History detail's.
import { For } from "../../lib/dom";
import { effect } from "../../lib/signal";
import { serverLabel } from "../../view/words";
import { Icon } from "./icon";

export interface ScopeServer {
  id: string;
  name: string;
  location?: string;
}

export function ServerScope(props: {
  label: string;
  servers: () => readonly ScopeServer[];
  /** The chosen id; "" is the aggregate. */
  value: () => string;
  onChange(id: string): void;
  /** The choice of every server at once, as "" in `value`. */
  aggregate?: () => string;
  disabled?: () => boolean;
  /** Servers listed but not choosable. */
  unavailable?: () => readonly string[];
  /** A lens over results: a server mark and a borderless field. */
  quiet?: boolean;
}): Node {
  const select = (
    <select
      class={props.quiet ? "server-scope quiet" : "server-scope"}
      aria-label={props.label}
      disabled={() => props.disabled?.() ?? false}
      onchange={(event) => props.onChange(event.currentTarget.value)}
    >
      {props.aggregate && <option value="">{props.aggregate}</option>}
      <For each={props.servers} by={(server) => server.id}>
        {(server) => (
          <option value={server().id} disabled={() => props.unavailable?.().includes(server().id) ?? false}>
            {() => serverLabel(server())}
          </option>
        )}
      </For>
    </select>
  ) as HTMLSelectElement;
  // After the rows' own update, so the chosen option exists.
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
