package goclient

import (
	"io"
	"strings"
	"testing"
)

func TestControlJSONBoundsAndSyntax(t *testing.T) {
	for _, body := range []string{`{} {}`, `{"a":1,"a":2}`, `"` + strings.Repeat("a", maxControlBytes-1) + `"`} {
		var out any
		if err := readControlJSON(strings.NewReader(body), &out); err == nil {
			t.Fatalf("accepted invalid control body of length %d", len(body))
		}
	}
	var out string
	if err := readControlJSON(strings.NewReader(`"`+strings.Repeat("a", maxControlBytes-2)+`"`), &out); err != nil {
		t.Fatal(err)
	}
}

type endlessControlReader struct{ read int }

func (r *endlessControlReader) Read(p []byte) (int, error) {
	clear(p)
	r.read += len(p)
	return len(p), nil
}

var _ io.Reader = (*endlessControlReader)(nil)

func TestControlJSONStopsReadingOversizedStream(t *testing.T) {
	body := &endlessControlReader{}
	var out any
	if err := readControlJSON(body, &out); err == nil {
		t.Fatal("accepted oversized stream")
	}
	if body.read != maxControlBytes+1 {
		t.Fatalf("read %d bytes, want %d", body.read, maxControlBytes+1)
	}
}
