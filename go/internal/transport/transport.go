// Package transport adapts concrete network channels to measurement operations.
package transport

import "net/http"

// Proto names the observed HTTP wire protocol.
type Proto string

const (
	ProtoH1 Proto = "http/1.1"
	ProtoH2 Proto = "h2"
	ProtoH3 Proto = "h3"
)

// HTTPProtocol reports the protocol actually used by a request.
func HTTPProtocol(r *http.Request) Proto {
	switch r.ProtoMajor {
	case 3:
		return ProtoH3
	case 2:
		return ProtoH2
	default:
		return ProtoH1
	}
}

// MessageBus is a message-delimited channel; adapters own cancellation and closure.
type MessageBus interface {
	Recv() (string, error)
	Send(string) error
}
