/* Submit feedback and smooth in-place errors for the server-rendered auth
   pages. A sign-in POST is otherwise a full navigation, and password
   verification is deliberately slow, so the button would sit unchanged and then
   its spinner would freeze the instant the browser commits the navigation.

   This marks the submitted form busy (the CSS shows a spinner) and, for the
   same-origin password and CLI-approval forms, submits with fetch: a rejection
   swaps the card in place instead of navigating, so the spinner never freezes,
   and a success follows through to the app. It degrades to a native submit
   whenever fetch or the DOM APIs are unavailable, and always for the OIDC form,
   which must navigate to the identity provider.

   The body is sent url-encoded (not multipart) because the server parses it
   with ParseForm. go/internal/auth serves this file verbatim and pins its
   sha256 in the Content-Security-Policy, so it must stay dependency-free. */
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

/** @param {HTMLFormElement} form */
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
    .then((response) =>
      response.text().then((html) => {
        const card = new DOMParser()
          .parseFromString(html, "text/html")
          .querySelector("main.card");
        const current = document.querySelector("main.card");
        if (card && current) {
          current.replaceWith(document.importNode(card, true));
          // Put the operator back on the field they'll retry in.
          const focus = document.querySelector("input[autofocus]");
          if (focus instanceof HTMLElement) focus.focus();
        } else if (response.ok) location.assign(response.url);
        else location.reload();
      }),
    )
    .catch(() => setBusy(form, false));
});
