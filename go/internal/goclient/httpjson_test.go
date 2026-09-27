package goclient

import (
	"strings"
	"testing"
)

type endlessControlReader struct{ read int }

func (r *endlessControlReader) Read(p []byte) (int, error) {
	clear(p)
	r.read += len(p)
	return len(p), nil
}

func TestControlJSONBounds(t *testing.T) {
	t.Parallel()
	var out string
	if err := readControlJSON(strings.NewReader(`"`+strings.Repeat("a", maxControlBytes-2)+`"`), &out); err != nil {
		t.Fatalf("a body of exactly %d bytes: %v", maxControlBytes, err)
	}
	for _, body := range []string{`"` + strings.Repeat("a", maxControlBytes-1) + `"`, `{not json`} {
		if err := readControlJSON(strings.NewReader(body), &out); err == nil {
			t.Errorf("accepted invalid control body of length %d", len(body))
		}
	}
	endless := &endlessControlReader{}
	if err := readControlJSON(endless, &out); err == nil || endless.read != maxControlBytes+1 {
		t.Fatalf("an endless body read %d bytes (%v), want a refusal after %d", endless.read, err, maxControlBytes+1)
	}
}
