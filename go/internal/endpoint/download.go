package endpoint

import (
	"context"
	"io"
	"net/http"
	"strconv"
)

// Download streams incompressible random bytes for the client's download measurement.
type Download struct {
	block []byte
	meter *Meter // optional verbose per-second logger; nil unless -verbose
}

const (
	defaultBytes int64 = 25 * 1024 * 1024        // 25 MiB when ?bytes= is absent
	maxBytes     int64 = 64 * 1024 * 1024 * 1024 // 64 GiB hard ceiling
)

// NewDownload builds the endpoint bound to the shared RNG block. meter may be nil (no verbose logging).
func NewDownload(block []byte, meter *Meter) *Download {
	return &Download{block: block, meter: meter}
}

// ServeHTTP streams the requested bytes; HEAD stops at the framing and generates nothing.
func (d *Download) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	n := parseBytes(r.URL.Query().Get("bytes"))
	h := w.Header()
	h.Set("Content-Type", "application/octet-stream")
	h.Set("Cache-Control", "no-store")
	h.Set("Content-Length", strconv.FormatInt(n, 10))
	if r.Method != http.MethodHead {
		_ = d.Stream(r.Context(), n, w)
	}
}

// Stream repeats the shared block into sink; cancellation or a failed write is the client leaving.
func (d *Download) Stream(ctx context.Context, n int64, sink io.Writer) error {
	d.meter.Open()
	defer d.meter.Close()

	block := d.block
	blockLen := int64(len(block))
	done := ctx.Done()
	var off int64
	for n > 0 {
		select {
		case <-done:
			return nil
		default:
		}
		chunk := min(blockLen-off, n)
		wrote, werr := sink.Write(block[off : off+chunk])
		d.meter.Add(wrote)
		n -= int64(wrote)
		off += int64(wrote)
		if off >= blockLen {
			off = 0
		}
		if werr != nil {
			return nil
		}
	}
	return nil
}

func parseBytes(raw string) int64 {
	if raw == "" {
		return defaultBytes
	}
	n, err := strconv.ParseInt(raw, 10, 64)
	if err != nil || n < 0 {
		return defaultBytes
	}
	return min(n, maxBytes)
}
