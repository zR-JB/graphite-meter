package auth

import (
	"errors"
	"net/http"
	"strings"
	"testing"
	"unicode/utf8"
)

func TestProviderClientRejectsRedirects(t *testing.T) {
	client := providerHTTPClient()
	req, _ := http.NewRequest(http.MethodGet, "http://provider.example/token", nil)
	if err := client.CheckRedirect(req, nil); !errors.Is(err, http.ErrUseLastResponse) {
		t.Fatalf("redirect error=%v", err)
	}
}

func TestSafeDisplayNameSanitizesAndBoundsProviderInput(t *testing.T) {
	if got := safeDisplayName("\x00\x7f\u009b"); got != "OIDC user" {
		t.Fatalf("empty sanitized name=%q, want fallback", got)
	}
	if got := safeDisplayName("Ann\u009b31m\u0085Lee"); got != "Ann31mLee" {
		t.Fatalf("name with C1 controls=%q, want them stripped", got)
	}
	for _, subject := range []string{"a\x1b", "a\x7f", "a\u009b", "a\u0085"} {
		if validSubject(subject) {
			t.Fatalf("subject %q with a control character was accepted", subject)
		}
	}
	if !validSubject("user@example.net") {
		t.Fatal("plain subject was refused")
	}
	got := safeDisplayName(strings.Repeat("界", 100))
	if !utf8.ValidString(got) || len(got) > 256 || got != strings.Repeat("界", 85) {
		t.Fatalf("bounded name has len=%d valid=%v, want 85 runes/255 bytes", len(got), utf8.ValidString(got))
	}
}
