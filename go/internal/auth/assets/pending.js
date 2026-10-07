/* Submit feedback, in-place errors, the host line and show-password keys for the server-rendered auth pages.
   A native sign-in POST leaves a dead spinner: password verification is
   deliberately slow, and the browser freezes animations the instant a
   navigation commits. Same-origin password and CLI-approval forms submit with
   fetch, so a rejection swaps the card in place and a success follows the
   redirect. go/internal/auth serves this file verbatim and pins its sha256 in
   the Content-Security-Policy, so it must stay dependency-free. */
const INPLACE = new Set([
  "/auth/password",
  "/auth/cli/approve",
  "/auth/browser/approve",
]);

/**
 * @param {HTMLFormElement} form
 * @param {boolean} busy
 */
function setBusy(form, busy) {
  if (busy) form.dataset.busy = "1";
  else delete form.dataset.busy;
  for (const button of form.querySelectorAll("button")) button.disabled = busy;
}

/**
 * What needs script in a card: the host it belongs to and the show-password keys.
 * @param {ParentNode} root
 */
function enhance(root) {
  for (const host of root.querySelectorAll("[data-host]"))
    host.textContent = location.host;
  for (const toggle of root.querySelectorAll("[data-reveal]"))
    if (toggle instanceof HTMLElement) toggle.hidden = false;
}

/**
 * The field a show-password key belongs to.
 * @param {EventTarget | null} target
 */
function revealed(target) {
  const toggle =
    target instanceof Element ? target.closest("[data-reveal]") : null;
  const field = toggle
    ? document.getElementById(toggle.getAttribute("data-reveal") ?? "")
    : null;
  return toggle && field instanceof HTMLInputElement ? { toggle, field } : null;
}

/**
 * @param {Element} toggle
 * @param {HTMLInputElement} field
 * @param {boolean} shown
 */
function reveal(toggle, field, shown) {
  field.type = shown ? "text" : "password";
  toggle.setAttribute("aria-pressed", String(shown));
  toggle.setAttribute("aria-label", shown ? "Hide password" : "Show password");
}

/**
 * Url-encoded body, not multipart: the server parses it with ParseForm.
 * @param {HTMLFormElement} form
 */
function encode(form) {
  const body = new URLSearchParams();
  new FormData(form).forEach((value, key) => {
    if (typeof value === "string") body.append(key, value);
  });
  return body;
}

/**
 * A rejection redirects back to the page being viewed (same path, error query),
 * a success to another page.
 * @param {Response} response
 * @param {{pathname: string}} here
 */
function leftThisPage(response, here) {
  return (
    response.redirected && new URL(response.url).pathname !== here.pathname
  );
}

// A press on the key keeps the caret, and a phone's keyboard, in the field.
document.addEventListener("pointerdown", (event) => {
  const pair = revealed(event.target);
  if (pair && document.activeElement === pair.field) event.preventDefault();
});
document.addEventListener("click", (event) => {
  const pair = revealed(event.target);
  if (pair) reveal(pair.toggle, pair.field, pair.field.type === "password");
});

document.addEventListener("submit", (event) => {
  const form = event.target;
  if (!(form instanceof HTMLFormElement) || form.dataset.busy) return;
  // A shown password goes back under its dots before it leaves, as a password manager expects.
  for (const toggle of form.querySelectorAll("[data-reveal]")) {
    const pair = revealed(toggle);
    if (pair) reveal(pair.toggle, pair.field, false);
  }
  setBusy(form, true);

  const action = new URL(form.action, location.href);
  const enhance =
    action.origin === location.origin &&
    INPLACE.has(action.pathname) &&
    typeof fetch === "function" &&
    typeof DOMParser === "function";
  if (!enhance) return; // native submit proceeds (e.g. OIDC → identity provider)

  event.preventDefault();
  fetch(action.href, {
    method: "POST",
    body: encode(form),
    credentials: "same-origin",
    redirect: "follow",
  })
    .then((response) => {
      // The app document goes unparsed: DOMParser checks its inline styles
      // against this page's strict CSP and logs a spurious violation.
      if (leftThisPage(response, location)) {
        location.assign(response.url);
        return;
      }
      return response.text().then((html) => {
        const page = new DOMParser().parseFromString(html, "text/html");
        const card = page.querySelector("main.card");
        const current = document.querySelector("main.card");
        if (card && current) {
          const next = document.importNode(card, true);
          current.replaceWith(next);
          enhance(next);
          document.title = page.title;
          // Inserted nodes ignore autofocus: focus the retry field, else the
          // heading, so the outcome is announced and focus stays on the card.
          const field = document.getElementById("password");
          const heading = document.querySelector("main h1");
          if (field) field.focus();
          else if (heading instanceof HTMLElement) {
            heading.tabIndex = -1;
            heading.focus();
          }
        } else if (response.ok) location.assign(response.url);
        else location.reload();
      });
    })
    .catch(() => setBusy(form, false));
});

enhance(document);
