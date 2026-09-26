package endpoint

import (
	"context"
	"io"
	"net/http"
	"strconv"
	"time"
)

type Download struct {
	block []byte
	meter *Meter // optional verbose per-second logger; nil unless -verbose
}

const (
	defaultBytes int64 = 25 * 1024 * 1024        // 25 MiB when ?bytes= is absent
	maxBytes     int64 = 64 * 1024 * 1024 * 1024 // 64 GiB hard ceiling
)

func NewDownload(block []byte, meter *Meter) *Download {
	return &Download{block: block, meter: meter}
}

func (d *Download) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	n := parseBytes(r.URL.Query().Get("bytes"))
	w.Header().Set("Content-Type", "application/octet-stream")
	w.Header().Set("Cache-Control", "no-store")
	w.Header().Set("Content-Length", strconv.FormatInt(n, 10))
	if r.Method != http.MethodHead {
		limit, _ := r.Context().Deadline()
		sink := &idleWriter{w: w, idle: idleDeadline{set: http.NewResponseController(w).SetWriteDeadline, limit: limit}}
		sink.idle.moved(time.Now())
		defer sink.idle.endWith(r.Context())()
		d.Stream(r.Context(), n, sink)
	}
}

// Stream repeats the shared block into sink; cancellation or a failed write is the client leaving.
func (d *Download) Stream(ctx context.Context, n int64, sink io.Writer) {
	d.meter.Open()
	defer d.meter.Close()
	block := d.block
	blockLen := int64(len(block))
	done := ctx.Done()
	var off int64
	for n > 0 {
		select {
		case <-done:
			return
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
			return
		}
	}
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
