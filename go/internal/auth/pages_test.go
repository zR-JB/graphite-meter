package auth

import (
	"crypto/sha256"
	"encoding/json/v2"
	"flag"
	"fmt"
	"os"
	"strings"
	"testing"
)

var updatePages = flag.Bool("update-pages", false, "rewrite shared page hashes from the Go templates")

// Both servers render these inputs. Go owns the expected hashes; inline assets are elided.
type pageCase struct {
	Name      string `json:"name"`
	Page      string `json:"page"`
	CSRF      string `json:"csrf,omitempty"`
	Provider  string `json:"provider,omitempty"`
	Challenge string `json:"challenge,omitempty"`
	Notice    string `json:"notice,omitempty"`
	Status    string `json:"status,omitempty"`
	Code      string `json:"code,omitempty"`
	Origin    string `json:"origin,omitempty"`
	Password  bool   `json:"password,omitzero"`
	OIDC      bool   `json:"oidc,omitzero"`
	OIDCReady bool   `json:"oidcReady,omitzero"`
	Browser   bool   `json:"browser,omitzero"`
	Capacity  bool   `json:"capacity,omitzero"`
	Opening   bool   `json:"opening,omitzero"`
	SHA256    string `json:"sha256"`
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
	const path = "testdata/pages.json"
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	var cases []pageCase
	if err := json.Unmarshal(data, &cases); err != nil || len(cases) == 0 {
		t.Fatalf("page fixtures: %v", err)
	}
	var output strings.Builder
	output.WriteString("[\n")
	for i := range cases {
		c := &cases[i]
		got := fmt.Sprintf("%x", sha256.Sum256([]byte(c.render(t))))
		if *updatePages {
			c.SHA256 = got
		} else if got != c.SHA256 {
			t.Errorf("%s is stale; run go test ./internal/auth -run TestPagesMatchSharedGoldens -update-pages", c.Name)
		}
		row, err := json.Marshal(c)
		if err != nil {
			t.Fatal(err)
		}
		if i > 0 {
			output.WriteString(",\n")
		}
		output.WriteString("  " + string(row))
	}
	if *updatePages {
		if err := os.WriteFile(path, []byte(output.String()+"\n]\n"), 0o644); err != nil {
			t.Fatal(err)
		}
	}
}
