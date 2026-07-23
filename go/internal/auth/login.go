package auth

import (
	"html/template"
	"net/http"
)

type loginView struct {
	Styles template.CSS
	// Notice is a validated notice code from a failed attempt, or "". Status is
	// a validated non-failure state ("expired", "signed_out") from a redirect to
	// the login page. The wording lives in the template.
	CSRF, Provider, Challenge, Notice, Status string
	Password, OIDC, OIDCReady                 bool
}

// parseStatus accepts only the non-failure states the login page renders, so
// nothing from the query string reaches the template unvalidated.
func parseStatus(raw string) string {
	switch raw {
	case "expired", "signed_out":
		return raw
	}
	return ""
}

func renderLogin(w http.ResponseWriter, tmpl *template.Template, data loginView) {
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	_ = tmpl.Execute(w, data)
}

// PreviewHandler renders the production login template with sample state.
// It is used only by the loopback development preview command.
func PreviewHandler(mode string, oidcReady bool) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/" && r.URL.Path != "/login" {
			http.NotFound(w, r)
			return
		}
		renderLogin(w, loginTemplate, loginView{
			Styles: authStyles, CSRF: "preview", Provider: "Authelia",
			Password: mode == "password" || mode == "hybrid",
			OIDC:     mode == "oidc" || mode == "hybrid", OIDCReady: oidcReady,
			Notice: string(parseNotice(r.URL.Query().Get("error"))), Status: parseStatus(r.URL.Query().Get("reason")),
		})
	})
}
