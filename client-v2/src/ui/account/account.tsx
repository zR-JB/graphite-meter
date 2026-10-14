// Who is signed in, and the two ways out: this session, or every session after a confirmation. Rendered only once
// the session answers; without it the meter works on, and the run reports a lost login itself.
import "./account.css";
import type { App } from "../../app/app";
import { csrfHeaders, type Account } from "../../engine/net/page";
import { Show } from "../../lib/dom";
import { computed, onCleanup, signal } from "../../lib/signal";
import { ConfirmDialog } from "../kit/dialog";
import { Icon } from "../kit/icon";
import { tip } from "../kit/tip";

export function AccountControl(props: { app: App }): Node {
  const account = signal<Account | null>(null);
  const request = new AbortController();
  let reading = false;
  const read = () => {
    if (reading || account.peek() || document.visibilityState === "hidden") return;
    reading = true;
    void props.app.session.account(request.signal).then((value) => {
      reading = false;
      if (!request.signal.aborted) account.set(value);
    });
  };
  read();
  // A read that failed is tried again when the page shows or the network returns, not left hidden for the tab's life.
  document.addEventListener("visibilitychange", read, { signal: request.signal });
  window.addEventListener("online", read, { signal: request.signal });
  onCleanup(() => request.abort());

  return (
    <Show when={account}>
      {(session) => {
        const local = computed(() => session().provider === "local");
        const label = computed(() => (local() ? "Local operator" : session().name));
        const provider = computed(() => (local() ? "Operator session" : session().provider));
        const confirming = signal(false);
        // A result the sign-out would leave unsaved: asked here, so staying is known and Sign out works again.
        const unsaved = signal(false);
        let form!: HTMLFormElement;
        let everywhere!: HTMLButtonElement;
        let csrf!: HTMLInputElement;
        let scope!: HTMLInputElement;
        let leaving = false;
        const leave = () => {
          csrf.value = csrfHeaders("POST", document.cookie)["X-CSRF-Token"] ?? csrf.value;
          form.submit();
        };
        return [
          <form
            class="account"
            method="post"
            action="/auth/logout"
            aria-label={() => `${label()}, ${provider()}`}
            // The page leaves once the run ended and its result's write landed, with the token as it is now: another
            // tab may have signed in again since this one read it. A submit drops its submitter, so the scope rides along.
            onsubmit={(event) => {
              event.preventDefault();
              if (leaving) return;
              leaving = true;
              scope.disabled = event.submitter !== everywhere;
              void props.app.signOut().then(() => {
                const { saver, run } = props.app;
                if (saver.pending.peek() || (saver.failed.peek() && run.result.peek())) unsaved.set(true);
                else leave();
              });
            }}
            ref={(node) => {
              form = node;
            }}
          >
            <input
              type="hidden"
              name="csrf"
              value={() => session().csrf}
              ref={(node) => {
                csrf = node;
              }}
            />
            <input
              type="hidden"
              name="scope"
              value="all"
              ref={(node) => {
                scope = node;
              }}
            />
            <div class="identity" ref={tip(() => `${label()}\n${provider()}`)}>
              <span class="avatar" aria-hidden="true">
                <Icon name="person" />
              </span>
              <strong class="name">{label}</strong>
            </div>
            <button
              class="btn btn-icon key signout everywhere"
              type="submit"
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
          <ConfirmDialog
            open={unsaved}
            id="sign-out-unsaved"
            title="Sign out without saving?"
            description="The result shown is not saved in History yet; signing out leaves it."
            cancel="Stay signed in"
            confirm="Sign out"
            onCancel={() => {
              unsaved.set(false);
              leaving = false;
              props.app.stay();
            }}
            onConfirm={() => {
              unsaved.set(false);
              leave();
            }}
          />,
        ];
      }}
    </Show>
  );
}
