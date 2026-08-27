package endpoint

import (
	"strconv"

	"github.com/zR-JB/graphite-meter/go/internal/transport"
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

func (d *Download) ID() string { return "download" }

// Handle streams ?bytes= of the shared block into the session's download sink, wrapping at the block end.
func (d *Download) Handle(s transport.Session) error {
	n := parseBytes(s.Query().Get("bytes"))

	// HTTP response setup is transport-specific; the byte streaming below is not.
	if w, _, ok := s.HTTP(); ok {
		h := w.Header()
		h.Set("Content-Type", "application/octet-stream")
		h.Set("Cache-Control", "no-store")
		h.Set("Content-Length", strconv.FormatInt(n, 10))
	}

	sink, err := s.OpenDownloadSink()
	if err != nil {
		return err
	}

	d.meter.Open()
	defer d.meter.Close()

	ctx := s.Context()
	block := d.block
	blockLen := int64(len(block))
	var off int64
	for n > 0 {
		select {
		case <-ctx.Done():
			return nil // client went away or request cancelled, not an error
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
			return nil // client disconnect mid-stream is normal; stop quietly
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
