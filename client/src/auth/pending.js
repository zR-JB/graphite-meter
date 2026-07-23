/* Submit feedback for the server-rendered auth pages. A sign-in POST is a full
   page navigation and password verification is deliberately slow, so without
   this the button sits unchanged for a second or more and invites a second
   click. Marks the form busy, disables its buttons against a double submit, and
   lets the CSS swap the label for a spinner.
   go/internal/auth serves this file verbatim and pins its sha256 in the
   Content-Security-Policy, so it must stay dependency-free. */
document.addEventListener("submit", (event) => {
  const form = event.target;
  if (!(form instanceof HTMLFormElement) || form.dataset.busy) return;
  form.dataset.busy = "1";
  for (const button of form.querySelectorAll("button")) button.disabled = true;
});
