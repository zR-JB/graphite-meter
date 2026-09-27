package auth

import (
	"strings"
	"testing"
	"unicode/utf8"
)

func TestSafeDisplayNameSanitizesAndBoundsProviderInput(t *testing.T) {
	if got := safeDisplayName("\x00\x7f\u009b"); got != "OIDC user" {
		t.Fatalf("empty sanitized name=%q, want fallback", got)
	}
	if validSubject("a\u009b") || !validSubject("user@example.net") {
		t.Fatal("a subject's control characters decide its validity")
	}
	got := safeDisplayName(strings.Repeat("界", 100))
	if !utf8.ValidString(got) || got != strings.Repeat("界", 63)+"…" {
		t.Fatalf("bounded name %q, want 63 runes and an ellipsis", got)
	}
}
