package goclient

import (
	"context"
	"fmt"
	"net/http"
	"sync"
	"sync/atomic"
	"time"
)

// A participant publishes counters and receiver identity; the coordinator chooses every rate window.
type participantCounters struct {
	mu       sync.Mutex
	down     *atomic.Uint64
	uploadID string
	progress *uploadProgress
}

func (p *participantCounters) attachDownload(total *atomic.Uint64) {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.down = total
}
func (p *participantCounters) attachUpload(id string, progress *uploadProgress) {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.uploadID = id
	p.progress = progress
}
func (p *participantCounters) download() uint64 {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.down != nil {
		return p.down.Load()
	}
	return 0
}
func (p *participantCounters) upload() (string, uint64, uint64) {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.progress == nil {
		return p.uploadID, 0, 0
	}
	bytes, nanos := p.progress.counters()
	return p.uploadID, bytes, nanos
}

func waitCoordinatedTransfer(ctx context.Context, laneErr, progressErr <-chan error) (Result, error) {
	select {
	case <-ctx.Done():
		return Result{}, context.Cause(ctx)
	case err := <-laneErr:
		return Result{}, err
	case err := <-progressErr:
		return Result{}, err
	}
}

func (r *runner) receiverCheckpoint(ctx context.Context, started time.Time) (*ReceiverSnapshot, error) {
	id, _, _ := r.coordinated.upload()
	if id == "" {
		return nil, fmt.Errorf("upload receiver is not ready")
	}
	endpoint, err := r.endpoint(r.routes().UploadCheckpoint)
	if err != nil {
		return nil, err
	}
	target := withUploadID(endpoint, id)
	requested := time.Since(started)
	var count struct {
		Bytes uint64 `json:"bytes"`
		Nanos uint64 `json:"nanos"`
	}
	if _, err := (jsonHTTPClient{r.http}).requestJSON(ctx, http.MethodPost, target, nil, http.Header{"Cache-Control": {"no-store"}}, &count, httpStatusError("receiver checkpoint")); err != nil {
		return nil, err
	}
	if count.Nanos == 0 || count.Nanos > uint64(1<<63-1) {
		return nil, fmt.Errorf("invalid receiver clock")
	}
	return &ReceiverSnapshot{ID: id, Bytes: count.Bytes, Nanos: count.Nanos, RequestedAt: requested, ReceivedAt: time.Since(started)}, nil
}
