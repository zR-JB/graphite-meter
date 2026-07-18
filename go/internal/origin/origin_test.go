package origin

import "testing"

func TestKeyNormalizesHTTPOrigins(t *testing.T) {
	for _, tc := range []struct{ a, b string }{
		{"https://Meter.Example:443", "https://meter.example"},
		{"http://meter.example:80", "http://meter.example"},
		{"https://[::1]:443", "https://[::1]"},
	} {
		if Key(tc.a) != tc.b || !Equal(tc.a, tc.b) {
			t.Errorf("Key(%q) = %q, want %q", tc.a, Key(tc.a), tc.b)
		}
	}
}

func TestKeyPreservesDistinctAndRelativeOrigins(t *testing.T) {
	if Equal("https://meter.example:444", "https://meter.example") {
		t.Fatal("non-default port collapsed")
	}
	if Key(".") != "." {
		t.Fatal("relative self marker changed")
	}
}
