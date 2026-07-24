package goclient

import (
	"context"
	"fmt"
	"sync"
	"sync/atomic"
	"time"

	"github.com/coder/websocket"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func (r *runner) measureLatency(ctx context.Context, stage string, underLoad bool, duration time.Duration, start <-chan struct{}) (LatencyStats, error) {
	if r.latencyTarget == nil {
		return LatencyStats{}, fmt.Errorf("no latency target selected")
	}
	u, err := wsEndpoint(r.latencyTarget.Origin, r.latencyTarget.Routes.Ping)
	if err != nil {
		return LatencyStats{}, err
	}
	conn, response, err := websocket.Dial(ctx, u, &websocket.DialOptions{HTTPClient: r.websocketHTTP, CompressionMode: websocket.CompressionDisabled})
	if err != nil {
		if authErr := authResponseError(response); authErr != nil {
			return LatencyStats{}, authErr
		}
		return LatencyStats{}, err
	}
	defer conn.Close(websocket.StatusNormalClosure, "")
	// A failed hello needs no handling here: the read goroutine below sees the
	// same broken connection and reports it through recvErr.
	_ = conn.Write(ctx, websocket.MessageText, []byte(wire.Encode(wire.Frame{Op: wire.OpHI, Proto: "ws"})))

	measureCtx, cancel := context.WithCancel(ctx)
	defer cancel()

	pending := make(map[uint32]time.Time)
	var mu sync.Mutex // guards pending and stats
	var nextID uint32
	stats := latencyStats{}
	recvErr := make(chan error, 1)
	var measuring atomic.Bool
	var measureTimer <-chan time.Time
	// The read goroutine outlives every return below, so stats must be
	// snapshotted under mu.
	snapshot := func() LatencyStats {
		mu.Lock()
		defer mu.Unlock()
		return stats.snapshot()
	}

	go func() {
		for {
			_, msg, err := conn.Read(measureCtx)
			if err != nil {
				recvErr <- err
				return
			}
			f, err := wire.Decode(string(msg))
			if err != nil {
				continue
			}
			if f.Op != wire.OpPONG {
				continue
			}
			now := time.Now()
			mu.Lock()
			sent, ok := pending[f.ID]
			if ok {
				delete(pending, f.ID)
			}
			if !ok || !measuring.Load() {
				mu.Unlock()
				continue
			}
			rtt := now.Sub(sent)
			stats.add(rtt, false)
			mu.Unlock()
			r.emit(Event{
				Kind:    EventLatency,
				At:      now,
				Stage:   stage,
				Latency: LatencySample{Stage: stage, RTT: rtt, UnderLoad: underLoad},
			})
		}
	}()

	ticker := time.NewTicker(r.cfg.PingInterval)
	defer ticker.Stop()
	timeoutTicker := time.NewTicker(50 * time.Millisecond)
	defer timeoutTicker.Stop()
	send := func() error {
		mu.Lock()
		id := nextID
		nextID++
		pending[id] = time.Now()
		mu.Unlock()
		return conn.Write(measureCtx, websocket.MessageText, []byte(wire.Encode(wire.Frame{Op: wire.OpPING, ID: id})))
	}
	if err := send(); err != nil {
		return LatencyStats{}, err
	}
	lossAfter := max(4*r.cfg.PingInterval, 250*time.Millisecond)
	for {
		select {
		case <-start:
			if measuring.Load() {
				continue
			}
			mu.Lock()
			pending = make(map[uint32]time.Time)
			mu.Unlock()
			measuring.Store(true)
			timer := time.NewTimer(duration)
			defer timer.Stop()
			measureTimer = timer.C
		case <-measureCtx.Done():
			// BYE releases the server's session promptly; the samples are
			// already collected, so a failed farewell changes nothing.
			_ = conn.Write(context.Background(), websocket.MessageText, []byte(wire.Encode(wire.Frame{Op: wire.OpBYE})))
			return snapshot(), nil
		case <-measureTimer:
			_ = conn.Write(context.Background(), websocket.MessageText, []byte(wire.Encode(wire.Frame{Op: wire.OpBYE})))
			return snapshot(), nil
		case err := <-recvErr:
			if measureCtx.Err() != nil {
				return snapshot(), nil
			}
			return LatencyStats{}, fmt.Errorf("latency WebSocket failed: %w", err)
		case <-ticker.C:
			if err := send(); err != nil {
				return LatencyStats{}, err
			}
		case now := <-timeoutTicker.C:
			if !measuring.Load() {
				continue
			}
			mu.Lock()
			for id, sent := range pending {
				if now.Sub(sent) >= lossAfter {
					delete(pending, id)
					stats.add(0, true)
					r.emit(Event{
						Kind:    EventLatency,
						At:      now,
						Stage:   stage,
						Latency: LatencySample{Stage: stage, UnderLoad: underLoad, Lost: true},
					})
				}
			}
			mu.Unlock()
		}
	}
}
