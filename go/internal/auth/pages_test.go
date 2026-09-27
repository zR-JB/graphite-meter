package auth

import (
	"encoding/json/v2"
	"flag"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

var updatePages = flag.Bool("update-pages", false, "rewrite testdata/pages from the templates")

// pageCase is the first line of a golden page the Rust server renders too; inline assets are elided.
type pageCase struct {
	Page      string `json:"page"`
	CSRF      string `json:"csrf"`
	Provider  string `json:"provider"`
	Challenge string `json:"challenge"`
	Notice    string `json:"notice"`
	Status    string `json:"status"`
	Code      string `json:"code"`
	Origin    string `json:"origin"`
	Password  bool   `json:"password"`
	OIDC      bool   `json:"oidc"`
	OIDCReady bool   `json:"oidcReady"`
	Browser   bool   `json:"browser"`
	Capacity  bool   `json:"capacity"`
	Opening   bool   `json:"opening"`
}

func (c pageCase) render(t *testing.T) string {
	var b strings.Builder
	var err error
	switch c.Page {
	case "login":
		err = loginTemplate.Execute(&b, loginView{Styles: authStyles, CSRF: c.CSRF, Provider: c.Provider,
			Challenge: c.Challenge, Notice: c.Notice, Status: c.Status, Password: c.Password, OIDC: c.OIDC,
			OIDCReady: c.OIDCReady})
	case "cli":
		err = cliTemplate.Execute(&b, map[string]any{"Styles": authStyles, "Code": c.Code, "Challenge": c.Challenge,
			"CSRF": c.CSRF, "BrowserOrigin": c.Origin, "BrowserCapacity": c.Capacity, "ClientLimit": maxSessionGrants})
	case "cli-done":
		err = cliDoneTemplate.Execute(&b, map[string]any{"Styles": authStyles, "Browser": c.Browser})
	case "continue":
		err = continueTemplate.Execute(&b, map[string]any{"Styles": authStyles, "Challenge": c.Challenge,
			"Opening": c.Opening})
	default:
		t.Fatalf("unknown page %q", c.Page)
	}
	if err != nil {
		t.Fatal(err)
	}
	return strings.NewReplacer(authCSS, "/* auth.css */", authThemeJS, "/* theme.js */",
		authPendingJS, "/* pending.js */").Replace(b.String())
}

func TestPagesMatchSharedGoldens(t *testing.T) {
	paths, err := filepath.Glob("testdata/pages/*.golden")
	if err != nil || len(paths) == 0 {
		t.Fatalf("golden pages: %v", err)
	}
	for _, path := range paths {
		data, err := os.ReadFile(path)
		if err != nil {
			t.Fatal(err)
		}
		header, want, _ := strings.Cut(string(data), "\n")
		var c pageCase
		if err := json.Unmarshal([]byte(header), &c); err != nil {
			t.Fatalf("%s: %v", path, err)
		}
		got := c.render(t)
		if *updatePages {
			if err := os.WriteFile(path, []byte(header+"\n"+got), 0o644); err != nil {
				t.Fatal(err)
			}
		} else if got != want {
			t.Errorf("%s is stale; run go test ./internal/auth -run TestPagesMatchSharedGoldens -update-pages", path)
		}
	}
}
