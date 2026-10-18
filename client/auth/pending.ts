// The auth pages' script: the host line, show-password keys and in-page forms; without it forms post natively.

import { AUTH_ROUTES } from "../src/protocol/routes";

type Page = Pick<
  Window & typeof globalThis,
  | "document"
  | "location"
  | "fetch"
  | "DOMParser"
  | "FormData"
  | "addEventListener"
  | "setTimeout"
  | "clearTimeout"
>;

// A native POST would freeze the busy animation through the slow password check; other forms submit natively.
const IN_PLACE = new Set<string>([
  AUTH_ROUTES.password,
  AUTH_ROUTES.cliApprove,
  AUTH_ROUTES.browserApprove,
]);
/** A post and its answer get this long; past it the form is usable again and says the outcome is unknown. */
const SUBMIT_MS = 20_000;
const UNREACHABLE = "Could not reach the server. Try again.";
const UNANSWERED =
  "The server did not answer in time. Reload to see whether it went through, or try again.";

export function install({
  document,
  location,
  fetch,
  DOMParser,
  FormData,
  addEventListener,
  setTimeout,
  clearTimeout,
}: Page): void {
  // From the address bar, so a visitor sees which server asks and the page discloses nothing.
  const wake = (root: ParentNode) => {
    for (const line of root.querySelectorAll("[data-host]")) line.textContent = location.host;
    for (const key of root.querySelectorAll<HTMLElement>("[data-reveal]")) key.hidden = false;
  };
  const fieldOf = (key: Element) =>
    document.getElementById(key.getAttribute("data-reveal")!) as HTMLInputElement;
  const show = (key: Element, shown: boolean) => {
    fieldOf(key).type = shown ? "text" : "password";
    key.setAttribute("aria-pressed", String(shown));
    key.setAttribute("aria-label", shown ? "Hide password" : "Show password");
  };
  // aria-disabled, not disabled: a disabled button would drop the focus it holds.
  const setBusy = (form: HTMLFormElement, busy: boolean) => {
    const submit = form.querySelector("[type=submit]");
    if (busy) {
      form.setAttribute("aria-busy", "true");
      submit?.setAttribute("aria-disabled", "true");
    } else {
      form.removeAttribute("aria-busy");
      submit?.removeAttribute("aria-disabled");
    }
  };
  // One fixed sentence above the form, which stays usable: nothing a failing hop answered is shown.
  const unreachable = (form: HTMLFormElement, text = UNREACHABLE) => {
    setBusy(form, false);
    const shown = form.previousElementSibling;
    if (shown?.hasAttribute("data-unreachable")) {
      shown.textContent = text;
      return;
    }
    const notice = document.createElement("p");
    notice.setAttribute("class", "notice");
    notice.setAttribute("data-tone", "warn");
    notice.setAttribute("data-unreachable", "");
    notice.setAttribute("role", "alert");
    notice.textContent = text;
    form.before(notice);
  };

  // A page restored from the back-forward cache keeps the busy form its submit left.
  addEventListener("pageshow", (event) => {
    if (event.persisted) for (const form of document.querySelectorAll("form")) setBusy(form, false);
  });

  // A press on the key keeps the caret, and a phone's keyboard, in the field.
  document.addEventListener("pointerdown", (event) => {
    const key = (event.target as Element).closest("[data-reveal]");
    if (key && document.activeElement === fieldOf(key)) event.preventDefault();
  });
  document.addEventListener("click", (event) => {
    const key = (event.target as Element).closest("[data-reveal]");
    if (key) show(key, fieldOf(key).type === "password");
  });

  document.addEventListener("submit", (event) => {
    const form = event.target as HTMLFormElement;
    if (form.hasAttribute("aria-busy")) {
      event.preventDefault();
      return;
    }
    // A shown password goes back under its dots before it leaves, as a password manager expects.
    for (const key of form.querySelectorAll("[data-reveal]")) show(key, false);
    setBusy(form, true);
    const action = new URL(form.action, location.href);
    if (action.origin !== location.origin || !IN_PLACE.has(action.pathname)) return;
    event.preventDefault();
    // URL-encoded, not multipart: the server reads only that.
    const body = new URLSearchParams();
    for (const [name, value] of new FormData(form))
      if (typeof value === "string") body.append(name, value);
    // The answer's body counts against the same bound; nothing is posted again on its own.
    const answer = new AbortController();
    const bound = setTimeout(() => answer.abort(), SUBMIT_MS);
    fetch(action, {
      method: "POST",
      body,
      credentials: "same-origin",
      redirect: "follow",
      signal: answer.signal,
    })
      .then(async (response) => {
        // A success leaves this path and goes unparsed: parsing the app would report CSP violations for its styles.
        if (response.redirected && new URL(response.url).pathname !== location.pathname) {
          location.assign(response.url);
          return;
        }
        const page = new DOMParser().parseFromString(await response.text(), "text/html");
        const card = page.querySelector("main.card");
        if (!card) {
          // A refusal without a card shows its page's state on a reload; a failing hop's answer is not this server's.
          if (response.ok) location.assign(response.url);
          else if (response.status < 500) location.reload();
          else unreachable(form);
          return;
        }
        const next = document.importNode(card, true);
        document.querySelector("main.card")!.replaceWith(next);
        wake(next);
        document.title = page.title;
        // Inserted nodes ignore autofocus: focus the retry field, else the heading, so the outcome is announced.
        const field = document.getElementById("password");
        const heading = next.querySelector("h1");
        if (field) field.focus();
        else if (heading) {
          heading.tabIndex = -1;
          heading.focus();
        }
      })
      .catch(() => unreachable(form, answer.signal.aborted ? UNANSWERED : UNREACHABLE))
      .finally(() => clearTimeout(bound));
  });

  wake(document);
}
