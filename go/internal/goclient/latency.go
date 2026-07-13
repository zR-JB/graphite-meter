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
	routes := r.routes()
	if routes.WebSocket == nil {
		return LatencyStats{}, fmt.Errorf("selected target has no latency websocket")
	}
	path := routes.WebSocket.Ping
	baseOrigin := r.cfg.BaseURL
	if r.target != nil {
		baseOrigin = r.target.Origin
	}
	u, err := wsEndpoint(baseOrigin, path)
	if err != nil {
		return LatencyStats{}, err
	}
	conn, _, err := websocket.Dial(ctx, u, &websocket.DialOptions{HTTPClient: r.websocketHTTP, CompressionMode: websocket.CompressionDisabled})
	if err != nil {
		return LatencyStats{}, err
	}
	defer conn.Close(websocket.StatusNormalClosure, "")
	_ = conn.Write(ctx, websocket.MessageText, []byte(wire.Encode(wire.Frame{Op: wire.OpHI, Proto: "ws"})))

	measureCtx, cancel := context.WithCancel(ctx)
	defer cancel()

	pending := make(map[uint32]time.Time)
	var mu sync.Mutex
	var nextID uint32
	stats := latencyStats{}
	recvErr := make(chan error, 1)
	var measuring atomic.Bool
	var measureTimer <-chan time.Time

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
			mu.Unlock()
			if !ok {
				continue
			}
			if !measuring.Load() {
				continue
			}
			rtt := now.Sub(sent)
			stats.add(rtt, false)
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
	lossAfter := maxDuration(4*r.cfg.PingInterval, 250*time.Millisecond)
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
			_ = conn.Write(context.Background(), websocket.MessageText, []byte(wire.Encode(wire.Frame{Op: wire.OpBYE})))
			return stats.snapshot(), nil
		case <-measureTimer:
			_ = conn.Write(context.Background(), websocket.MessageText, []byte(wire.Encode(wire.Frame{Op: wire.OpBYE})))
			return stats.snapshot(), nil
		case err := <-recvErr:
			if measureCtx.Err() != nil {
				return stats.snapshot(), nil
			}
			return LatencyStats{}, fmt.Errorf("latency websocket failed: %w", err)
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

func maxDuration(a, b time.Duration) time.Duration {
	if a > b {
		return a
	}
	return b
}
