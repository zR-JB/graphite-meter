package endpoint

import (
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// ServePing echoes each probe with its application handling duration until
// recv or send fails; the adapter owns the channel's lifetime. The measured
// interval starts once recv has returned a message and ends before the reply
// is encoded, so it excludes transport receive work and every queue before it.
// recv's buffer may be reused on the next call; send must not retain its argument.
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
