package endpoint

import (
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// ServePing echoes each probe with its handling time, measured from recv's return to before
// encoding, until recv or send fails. recv may reuse its buffer; send must not retain its argument.
func ServePing(recv func() ([]byte, error), send func([]byte) error) {
	var reply [wire.MaxPongLen]byte
	for {
		message, err := recv()
		if err != nil {
			return
		}
		receivedAt := time.Now()
		id, err := wire.DecodePing(string(message))
		if err != nil {
			continue
		}
		handling := uint64(time.Since(receivedAt).Nanoseconds()) //nosec G115 -- monotonic elapsed duration
		if send(wire.AppendPong(reply[:0], id, handling)) != nil {
			return
		}
	}
}
