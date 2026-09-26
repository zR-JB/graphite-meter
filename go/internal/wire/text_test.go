package wire

import "testing"

func TestTextPolicyKeepsTerminalControlsOut(t *testing.T) {
	t.Parallel()
	for _, c := range []struct {
		in    string
		limit int
		clean string
		safe  bool
	}{
		{"Frankfurt · DE", 64, "Frankfurt · DE", true},
		{"osc\x1b]52;c;cHduZWQ=\a", 64, "osc ]52;c;cHduZWQ= ", false},
		{"c1\u009b2J", 64, "c1 2J", false},
		{"del\x7f", 64, "del ", false},
		{"two\nlines", 64, "two lines", false},
		{"bad\xffutf8", 64, "bad�utf8", false},
		{"evil\u202egnp.exe", 64, "evil gnp.exe", false},
		{"iso\u2066late\u2069", 64, "iso late ", false},
		{"arabic\u061cmark", 64, "arabic mark", false},
		{"123456789", 5, "1234…", true},
	} {
		if got := CleanText(c.in, c.limit); got != c.clean {
			t.Errorf("CleanText(%q) = %q, want %q", c.in, got, c.clean)
		}
		if SafeText(c.in) != c.safe {
			t.Errorf("SafeText(%q) = %v, want %v", c.in, !c.safe, c.safe)
		}
	}
}

func TestUploadRefusalDetailIsCleanedAtDecode(t *testing.T) {
	t.Parallel()
	event, err := DecodeUploadProgress(
		[]byte(`{"type":"error","message":"busy\u001b]52;c;eA==\u0007","code":"x\u009b2J"}`))
	if err != nil || event.Message != "busy ]52;c;eA== " || event.Code != "x 2J" {
		t.Fatalf("refusal = %+v, %v", event, err)
	}
}
