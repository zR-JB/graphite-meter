// Who is signed in, and the two ways out: this session, or every session after a confirmation. Rendered only once
// the session answers; without it the meter works on, and the run reports a lost login itself.
import "./account.css";
import type { App } from "../../app/app";
import type { Account } from "../../engine/net/page";
import { Show } from "../../lib/dom";
import { computed, onCleanup, signal } from "../../lib/signal";
import { ConfirmDialog } from "../kit/dialog";
import { Icon } from "../kit/icon";
import { tip } from "../kit/tip";

export function AccountControl(props: { app: App }): Node {
  const account = signal<Account | null>(null);
  const request = new AbortController();
  void props.app.session.account(request.signal).then((value) => {
    if (!request.signal.aborted) account.set(value);
  });
  onCleanup(() => request.abort());

  return (
    <Show when={account}>
      {(session) => {
        const local = computed(() => session().provider === "local");
        const label = computed(() => (local() ? "Local operator" : session().name));
        const provider = computed(() => (local() ? "Operator session" : session().provider));
        const confirming = signal(false);
        let form!: HTMLFormElement;
        let everywhere!: HTMLButtonElement;
        return [
          <form
            class="account"
            method="post"
            action="/auth/logout"
            aria-label={() => `${label()}, ${provider()}`}
            // A deliberate sign-out is no lapsed login.
            onsubmit={() => props.app.signOut()}
            ref={(node) => {
              form = node;
            }}
          >
            <input type="hidden" name="csrf" value={() => session().csrf} />
            <div class="identity" ref={tip(() => `${label()}\n${provider()}`)}>
              <span class="avatar" aria-hidden="true">
                <Icon name="person" />
              </span>
              <strong class="name">{label}</strong>
            </div>
            <button
              class="btn btn-icon key signout everywhere"
              type="submit"
              name="scope"
              value="all"
              aria-label={() => `Sign out ${label()} everywhere`}
              ref={(node) => {
                everywhere = node;
                tip(() => "End all sessions for this account")(node);
              }}
              onclick={(event) => {
                event.preventDefault();
                confirming.set(true);
              }}
            >
              <Icon name="power" />
            </button>
            <button
              class="btn btn-icon key signout"
              type="submit"
              aria-label={() => `Sign out ${label()}`}
              ref={tip(() => "Sign out")}
            >
              <Icon name="exit" />
            </button>
          </form>,
          <ConfirmDialog
            open={confirming}
            id="sign-out-everywhere"
            invoker={() => everywhere}
            title="Sign out everywhere?"
            description={() => `End every session of ${label()} on all devices, including this one.`}
            cancel="Stay signed in"
            confirm="Sign out everywhere"
            onCancel={() => confirming.set(false)}
            onConfirm={() => {
              confirming.set(false);
              form.requestSubmit(everywhere);
            }}
          />,
        ];
      }}
    </Show>
  );
}
