// Package endpoint implements the measurement operations behind each route.
// The server composes their handlers with authentication, admission and the
// transport adapters; nothing here decides who may reach them.
package endpoint

import (
	"context"
	"io"
)

// StreamFunc writes n bytes of download payload to w until ctx ends. The caller owns w's cancellation and closure.
type StreamFunc func(ctx context.Context, n int64, w io.Writer) error

// ReceiveFunc counts src into the upload receiver id held by owner. The caller owns src's cancellation.
type ReceiveFunc func(ctx context.Context, id, owner string, src io.Reader) (int64, error)
