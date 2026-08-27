package auth

import (
	"html/template"
	"net/http"
)

type loginView struct {
	Styles                                    template.CSS
	CSRF, Provider, Challenge, Notice, Status string
	Password, OIDC, OIDCReady                 bool
}

func parseStatus(raw string) string {
	switch raw {
	case "expired", "renew", "signed_out":
		return raw
	}
	return ""
}

func renderLogin(w http.ResponseWriter, tmpl *template.Template, data loginView) {
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	_ = tmpl.Execute(w, data)
}

func PreviewHandler(mode string, oidcReady bool) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/" && r.URL.Path != "/login" {
			http.NotFound(w, r)
			return
		}
		password, oidc := authModes(mode)
		renderLogin(w, loginTemplate, loginView{
			Styles: authStyles, CSRF: "preview", Provider: "Authelia",
			Password: password, OIDC: oidc, OIDCReady: oidcReady,
			Notice: string(parseNotice(r.URL.Query().Get("error"))), Status: parseStatus(r.URL.Query().Get("reason")),
		})
	})
}
