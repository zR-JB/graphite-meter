package auth

import (
	"crypto/sha256"
	"encoding/base64"
	"html/template"
)

var (
	authStyles       = template.CSS(authCSS)
	loginTemplate    = page("login", loginHTML)
	cliTemplate      = page("cli", cliHTML)
	cliDoneTemplate  = page("cli-done", cliDoneHTML)
	continueTemplate = page("continue", continueHTML)
	authStyleHash    = cspHash(authCSS)
	authThemeHash    = cspHash(authThemeJS)
	authPendingHash  = cspHash(authPendingJS)
)

// page parses an auth page together with the "theme" and "pending" templates
// its markup includes. Each script arrives as a template.JS value because
// html/template emits those byte for byte, while comments in literal script
// text are dropped by its JS lexer — the served bytes must match the digests
// authThemeHash and authPendingHash pin.
func page(name, text string) *template.Template {
	scripts := template.FuncMap{
		"themeJS":   func() template.JS { return template.JS(authThemeJS) },
		"pendingJS": func() template.JS { return template.JS(authPendingJS) },
	}
	set := template.Must(template.New("theme").Funcs(scripts).Parse(`<script>{{themeJS}}</script>`))
	template.Must(set.New("pending").Parse(`<script>{{pendingJS}}</script>`))
	return template.Must(set.New(name).Parse(text))
}

// cspHash is the digest form a CSP 'sha256-…' source expects: base64 of the
// raw sha256 over the element's exact text content.
func cspHash(asset string) string {
	sum := sha256.Sum256([]byte(asset))
	return base64.StdEncoding.EncodeToString(sum[:])
}
