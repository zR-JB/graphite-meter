// Package endpoint implements the measurement operations behind each route.
package endpoint

import (
	"context"
	"io"
)

// StreamFunc writes n download bytes to w until ctx ends.
type StreamFunc func(ctx context.Context, n int64, w io.Writer) error

// ReceiveFunc counts src into owner's upload receiver id.
type ReceiveFunc func(ctx context.Context, id, owner string, src io.Reader) (int64, error)
