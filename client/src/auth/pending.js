/* Submit feedback and in-place errors for the server-rendered auth pages.
   A native sign-in POST leaves a dead spinner: password verification is
   deliberately slow, and the browser freezes animations the instant a
   navigation commits. Same-origin password and CLI-approval forms submit with
   fetch, so a rejection swaps the card in place and a success follows the
   redirect. go/internal/auth serves this file verbatim and pins its sha256 in
   the Content-Security-Policy, so it must stay dependency-free. */
const INPLACE = new Set(["/auth/password", "/auth/cli/approve"]);

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

document.addEventListener("submit", (event) => {
  const form = event.target;
  if (!(form instanceof HTMLFormElement) || form.dataset.busy) return;
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
      const redirectedToApp =
        response.redirected &&
        new URL(response.url).pathname !== action.pathname;
      // The app document goes unparsed: DOMParser checks its inline styles
      // against this page's strict CSP and logs a spurious violation.
      if (redirectedToApp) {
        location.assign(response.url);
        return;
      }
      return response.text().then((html) => {
        const card = new DOMParser()
          .parseFromString(html, "text/html")
          .querySelector("main.card");
        const current = document.querySelector("main.card");
        if (card && current) {
          current.replaceWith(document.importNode(card, true));
          // Inserted nodes ignore autofocus, so focus the retry field here.
          const focus = document.querySelector("input[autofocus]");
          if (focus instanceof HTMLElement) focus.focus();
        } else if (response.ok) location.assign(response.url);
        else location.reload();
      });
    })
    .catch(() => setBusy(form, false));
});
