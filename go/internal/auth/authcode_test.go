package auth

import "testing"

func TestValidAuthCode(t *testing.T) {
	cases := []struct {
		name string
		code string
		want bool
	}{
		{"opaque random", "SplxlOBeZQQYbYS6WxSbIA", true},
		{"jwt shaped", "eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiIxIn0.c2ln", true},
		{"full vschar span", " !~/+=._-", true},
		{"empty", "", false},
		{"embedded nul", "abc\x00def", false},
		{"newline", "abc\ndef", false},
		{"tab", "abc\tdef", false},
		{"del", "abc\x7fdef", false},
		{"non ascii", "abcé", false},
		{"high byte", "abc\x80", false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if got := validAuthCode(tc.code); got != tc.want {
				t.Fatalf("validAuthCode(%q) = %v, want %v", tc.code, got, tc.want)
			}
		})
	}
}
