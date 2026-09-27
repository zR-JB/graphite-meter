package auth

import "testing"

func TestValidAuthCode(t *testing.T) {
	for code, want := range map[string]bool{
		"SplxlOBeZQQYbYS6WxSbIA": true,
		" !~/+=._-":              true,
		"":                       false,
		"abc\x00def":             false,
		"abc\x7fdef":             false,
		"abc\x80":                false,
	} {
		if got := validAuthCode(code); got != want {
			t.Errorf("validAuthCode(%q) = %v, want %v", code, got, want)
		}
	}
}
