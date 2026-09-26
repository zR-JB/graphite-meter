package auth

import (
	"crypto/sha256"
	_ "embed"
	"encoding/base64"
	"html/template"
)

var (
	//go:embed assets/auth.css
	authCSS string
	//go:embed assets/theme.js
	authThemeJS string
	//go:embed assets/pending.js
	authPendingJS string
	//go:embed assets/login.tmpl
	loginHTML string
	//go:embed assets/cli.tmpl
	cliHTML string
	//go:embed assets/cli-done.tmpl
	cliDoneHTML string
	//go:embed assets/continue.tmpl
	continueHTML string
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

func page(name, text string) *template.Template {
	scripts := template.FuncMap{
		"themeJS":   func() template.JS { return template.JS(authThemeJS) },
		"pendingJS": func() template.JS { return template.JS(authPendingJS) },
	}
	set := template.Must(template.New("theme").Funcs(scripts).Parse(`<script>{{themeJS}}</script>`))
	template.Must(set.New("pending").Parse(`<script>{{pendingJS}}</script>`))
	return template.Must(set.New(name).Parse(text))
}

func cspHash(asset string) string {
	sum := sha256.Sum256([]byte(asset))
	return base64.StdEncoding.EncodeToString(sum[:])
}
