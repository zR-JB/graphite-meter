package wire

import (
	"errors"
	"strconv"
	"strings"
)

// Pong pairs a client-owned probe ID with the server's handling duration in nanoseconds.
type Pong struct {
	ID            uint32
	HandlingNanos uint64
}

var errMalformedProbe = errors.New("malformed ping protocol message")

// DecodePing accepts exactly PING,<uint32 decimal ID>.
func DecodePing(message string) (uint32, error) {
	value, ok := strings.CutPrefix(message, "PING,")
	if !ok || len(value) > 10 {
		return 0, errMalformedProbe
	}
	id, err := strconv.ParseUint(value, 10, 32)
	if err != nil {
		return 0, errMalformedProbe
	}
	return uint32(id), nil
}

// DecodePong accepts exactly PONG,<uint32 decimal ID>,<uint64 decimal handling ns>.
func DecodePong(message string) (Pong, error) {
	values, ok := strings.CutPrefix(message, "PONG,")
	if !ok {
		return Pong{}, errMalformedProbe
	}
	idText, handlingText, ok := strings.Cut(values, ",")
	if !ok || len(idText) > 10 || len(handlingText) > 20 {
		return Pong{}, errMalformedProbe
	}
	id, err := strconv.ParseUint(idText, 10, 32)
	if err != nil {
		return Pong{}, errMalformedProbe
	}
	handling, err := strconv.ParseUint(handlingText, 10, 64)
	if err != nil {
		return Pong{}, errMalformedProbe
	}
	return Pong{ID: uint32(id), HandlingNanos: handling}, nil
}

func EncodePing(id uint32) string {
	return "PING," + strconv.FormatUint(uint64(id), 10)
}

func EncodePong(id uint32, handlingNanos uint64) string {
	return "PONG," + strconv.FormatUint(uint64(id), 10) + "," + strconv.FormatUint(handlingNanos, 10)
}
