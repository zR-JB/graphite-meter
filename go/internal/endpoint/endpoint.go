// Package endpoint implements HTTP routes and shared measurement operations.
package endpoint

import (
	"context"
	"io"
	"net/http"

	"github.com/zR-JB/graphite-meter/go/internal/transport"
)

// HTTPHandler handles one request; the registry supplies common response headers and error reporting.
type HTTPHandler interface {
	HandleHTTP(http.ResponseWriter, *http.Request) error
}

// MessageHandler processes a transport-owned message channel until it closes.
type MessageHandler interface {
	HandleMessages(context.Context, transport.MessageBus) error
}

// DownloadHandler produces a bounded byte stream. The adapter owns stream cancellation and closure.
type DownloadHandler interface {
	HandleDownload(context.Context, int64, io.Writer) error
}

// UploadHandler counts received bytes for an explicitly identified owner. The adapter owns I/O cancellation.
type UploadHandler interface {
	HandleUpload(context.Context, string, string, io.Reader) (int64, error)
}
