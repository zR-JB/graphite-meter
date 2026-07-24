package origin

import "testing"

func TestKeyNormalizesCasingAndDefaultPorts(t *testing.T) {
	for _, tc := range []struct{ raw, want string }{
		{"https://Meter.Example:443", "https://meter.example"},
		{"http://meter.example:80", "http://meter.example"},
		{"https://[::1]:443", "https://[::1]"},
	} {
		if got := Key(tc.raw); got != tc.want {
			t.Errorf("Key(%q) = %q, want %q", tc.raw, got, tc.want)
		}
		if !Equal(tc.raw, tc.want) {
			t.Errorf("Equal(%q, %q) = false, want true", tc.raw, tc.want)
		}
	}
}

func TestKeyPreservesDistinctAndRelativeOrigins(t *testing.T) {
	if Equal("https://meter.example:444", "https://meter.example") {
		t.Errorf("Equal(%q, %q) = true, want false", "https://meter.example:444", "https://meter.example")
	}
	if got := Key("."); got != "." {
		t.Errorf("Key(%q) = %q, want %q", ".", got, ".")
	}
}
