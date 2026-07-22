package auth

import (
	"crypto/sha256"
	"encoding/base64"
	"html/template"
)

var (
	authStyles      = template.CSS(authCSS)
	loginTemplate   = template.Must(template.New("login").Parse(loginHTML))
	cliTemplate     = template.Must(template.New("cli").Parse(cliHTML))
	cliDoneTemplate = template.Must(template.New("cli-done").Parse(cliDoneHTML))
	authStyleHash   = func() string {
		sum := sha256.Sum256([]byte(authCSS))
		return base64.StdEncoding.EncodeToString(sum[:])
	}()
)
