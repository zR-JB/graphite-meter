// The sign-in and approval pages' script: the host line, the show-password keys, and forms that stay on the page
// while the server checks them. Without it every form still posts natively.

type Page = Pick<Window & typeof globalThis, "document" | "location" | "fetch" | "DOMParser" | "FormData">;

// A native sign-in POST leaves a dead spinner: password verification is deliberately slow, and the browser freezes
// animations the instant a navigation commits. These forms post with fetch, so a refusal swaps the card in place and a
// success follows the redirect; any other form, such as a provider's sign-in, submits natively.
const IN_PLACE = new Set(["/auth/password", "/auth/cli/approve", "/auth/browser/approve"]);

/** A refusal redirects back to the page in view (same path, an error query); a success to another page. */
export function leftThisPage(response: Pick<Response, "redirected" | "url">, here: Pick<Location, "pathname">) {
  return response.redirected && new URL(response.url).pathname !== here.pathname;
}

export function install({ document, location, fetch, DOMParser, FormData }: Page): void {
  // Filled from the address bar, so a visitor sees which server asks, and nothing is disclosed.
  const wake = (root: ParentNode) => {
    for (const line of root.querySelectorAll("[data-host]")) line.textContent = location.host;
    for (const key of root.querySelectorAll<HTMLElement>("[data-reveal]")) key.hidden = false;
  };
  const fieldOf = (key: Element) => document.getElementById(key.getAttribute("data-reveal")!) as HTMLInputElement;
  const show = (key: Element, shown: boolean) => {
    fieldOf(key).type = shown ? "text" : "password";
    key.setAttribute("aria-pressed", String(shown));
    key.setAttribute("aria-label", shown ? "Hide password" : "Show password");
  };
  const setBusy = (form: HTMLFormElement, busy: boolean) => {
    if (busy) form.setAttribute("data-busy", "1");
    else form.removeAttribute("data-busy");
    for (const button of form.querySelectorAll("button")) button.disabled = busy;
  };

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
    if (form.hasAttribute("data-busy")) {
      event.preventDefault();
      return;
    }
    // A shown password goes back under its dots before it leaves, as a password manager expects.
    for (const key of form.querySelectorAll("[data-reveal]")) show(key, false);
    setBusy(form, true);
    const action = new URL(form.action, location.href);
    if (action.origin !== location.origin || !IN_PLACE.has(action.pathname)) return;
    event.preventDefault();
    // URL-encoded, not multipart: both servers read only that.
    const body = new URLSearchParams();
    for (const [name, value] of new FormData(form)) if (typeof value === "string") body.append(name, value);
    fetch(action, { method: "POST", body, credentials: "same-origin", redirect: "follow" })
      .then(async (response) => {
        // The app document goes unparsed: DOMParser checks its inline styles against this page's strict CSP and
        // logs a spurious violation.
        if (leftThisPage(response, location)) {
          location.assign(response.url);
          return;
        }
        const page = new DOMParser().parseFromString(await response.text(), "text/html");
        const card = page.querySelector("main.card");
        if (!card) {
          if (response.ok) location.assign(response.url);
          else location.reload();
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
      .catch(() => setBusy(form, false));
  });

  wake(document);
}
