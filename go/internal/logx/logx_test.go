package logx

import (
	"testing"
	"time"
)

func TestLinesAlignTheirColumnsAndEscapeControlCharacters(t *testing.T) {
	at := time.Date(2026, 10, 6, 16, 31, 2, 0, time.FixedZone("", 2*3600))
	got := Line(Warn, "tls", "peer sent \x1b[2J; ignored", at, false)
	want := "2026-10-06T16:31:02+02:00 WARN  tls:       peer sent \\x1b[2J; ignored\n"
	if got != want {
		t.Fatalf("plain line\n got %q\nwant %q", got, want)
	}
	coloured := Line(Warn, "tls", "certificate expires in 3 days; renew it", at, true)
	if want := "\x1b[1;36mhelp:\x1b[0m renew it\n"; coloured[len(coloured)-len(want):] != want {
		t.Fatalf("a warning's advice stands on its own help line: %q", coloured)
	}
}
