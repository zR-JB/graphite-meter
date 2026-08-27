package goclient

import (
	"context"
	"errors"
	"fmt"
	"maps"
	"sync"
	"sync/atomic"
	"time"

	"github.com/coder/websocket"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

type pingBus interface {
	Send(ctx context.Context, msg string) error
	Recv(ctx context.Context) (string, error)
	Close()
}

type wsBus struct{ conn *websocket.Conn }

func (b wsBus) Send(ctx context.Context, msg string) error {
	return b.conn.Write(ctx, websocket.MessageText, []byte(msg))
}

func (b wsBus) Recv(ctx context.Context) (string, error) {
	_, msg, err := b.conn.Read(ctx)
	return string(msg), err
}

func (b wsBus) Close() { b.conn.Close(websocket.StatusNormalClosure, "") } //nolint:errcheck // the samples are already collected

func (r *runner) dialPingBus(ctx context.Context) (pingBus, string, error) {
	if r.latencyTarget.Transport == wire.TransportWebTransport {
		sess, err := wtDial(ctx, r.cfg, r.latencyTarget.Origin, r.latencyTarget.Routes.WTPing, nil)
		if err != nil {
			return nil, "wt", err
		}
		return wtBus{sess: sess}, "wt", nil
	}
	u, err := wsEndpoint(r.latencyTarget.Origin, r.latencyTarget.Routes.Ping)
	if err != nil {
		return nil, "ws", err
	}
	conn, response, err := websocket.Dial(ctx, u, &websocket.DialOptions{HTTPClient: r.websocketHTTP, CompressionMode: websocket.CompressionDisabled})
	if err != nil {
		if authErr := authResponseError(response); authErr != nil {
			return nil, "ws", authErr
		}
		return nil, "ws", err
	}
	return wsBus{conn: conn}, "ws", nil
}

const busRedialWindow = 2 * time.Second

func (r *runner) redialPingBus(ctx context.Context, deadline time.Time) (pingBus, string, error) {
	redialCtx, cancel := context.WithDeadline(ctx, deadline)
	defer cancel()
	var lastErr error
	for {
		dialCtx, dialCancel := context.WithTimeout(redialCtx, 3*time.Second)
		bus, proto, err := r.dialPingBus(dialCtx)
		dialCancel()
		if err == nil {
			return bus, proto, nil
		}
		if _, authRequired := errors.AsType[*AuthRequiredError](err); authRequired {
			return nil, proto, err
		}
		if !errors.Is(err, context.DeadlineExceeded) || lastErr == nil {
			lastErr = err
		}
		select {
		case <-redialCtx.Done():
			if ctx.Err() != nil {
				return nil, proto, ctx.Err()
			}
			if lastErr != nil {
				return nil, proto, fmt.Errorf("latency channel not reconnected within %v: %w", busRedialWindow, lastErr)
			}
			return nil, proto, redialCtx.Err()
		case <-time.After(wtRedialBackoff):
		}
	}
}

func (r *runner) measureLatency(ctx context.Context, stage string, underLoad bool, duration time.Duration, start <-chan struct{}) (LatencyStats, error) {
	if r.latencyTarget == nil {
		return LatencyStats{}, fmt.Errorf("no latency target selected")
	}
	conn, proto, err := r.dialPingBus(ctx)
	if err != nil {
		return LatencyStats{}, err
	}
	defer func() { conn.Close() }()
	_ = conn.Send(ctx, wire.Encode(wire.Frame{Op: wire.OpHI, Proto: proto}))

	measureCtx, cancel := context.WithCancel(ctx)
	defer cancel()

	pending := make(map[uint32]time.Time)
	var mu sync.Mutex // guards pending and stats
	var nextID uint32
	stats := latencyStats{}
	recvErr := make(chan error, 1)
	var everPong atomic.Bool
	var measuring atomic.Bool
	var measureTimer <-chan time.Time
	snapshot := func() LatencyStats {
		mu.Lock()
		defer mu.Unlock()
		return stats.snapshot()
	}
	finish := func() (LatencyStats, error) {
		_ = conn.Send(context.Background(), wire.Encode(wire.Frame{Op: wire.OpBYE}))
		return snapshot(), nil
	}

	readLoop := func(bus pingBus) {
		for {
			msg, err := bus.Recv(measureCtx)
			if err != nil {
				recvErr <- err
				return
			}
			f, err := wire.Decode(msg)
			if err != nil {
				continue
			}
			if f.Op != wire.OpPONG {
				continue
			}
			everPong.Store(true)
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
	}
	go readLoop(conn)

	ticker := time.Tick(r.cfg.PingInterval)
	timeoutTicker := time.Tick(50 * time.Millisecond)
	send := func() error {
		mu.Lock()
		id := nextID
		nextID++
		pending[id] = time.Now()
		mu.Unlock()
		return conn.Send(measureCtx, wire.Encode(wire.Frame{Op: wire.OpPING, ID: id}))
	}
	if err := send(); err != nil {
		return LatencyStats{}, err
	}
	lossAfter := max(4*r.cfg.PingInterval, 250*time.Millisecond)
	for {
		select {
		case <-start:
			start = nil
			mu.Lock()
			clear(pending)
			mu.Unlock()
			measuring.Store(true)
			timer := time.NewTimer(duration)
			defer timer.Stop()
			measureTimer = timer.C
		case <-measureCtx.Done():
			return finish()
		case <-measureTimer:
			return finish()
		case err := <-recvErr:
			if measureCtx.Err() != nil {
				return snapshot(), nil
			}
			if !everPong.Load() {
				return LatencyStats{}, fmt.Errorf("latency channel failed: %w", err)
			}
			conn.Close()
			fresh, freshProto, dialErr := r.redialPingBus(measureCtx, time.Now().Add(busRedialWindow))
			if dialErr != nil {
				if measureCtx.Err() != nil {
					return snapshot(), nil
				}
				return LatencyStats{}, fmt.Errorf("latency channel failed: %w", dialErr)
			}
			conn, proto = fresh, freshProto
			mu.Lock()
			clear(pending)
			mu.Unlock()
			_ = conn.Send(measureCtx, wire.Encode(wire.Frame{Op: wire.OpHI, Proto: proto}))
			go readLoop(conn)
		case <-ticker:
			_ = send()
		case now := <-timeoutTicker:
			if !measuring.Load() {
				continue
			}
			mu.Lock()
			maps.DeleteFunc(pending, func(_ uint32, sent time.Time) bool {
				if now.Sub(sent) < lossAfter {
					return false
				}
				stats.add(0, true)
				r.emit(Event{
					Kind:    EventLatency,
					At:      now,
					Stage:   stage,
					Latency: LatencySample{Stage: stage, UnderLoad: underLoad, Lost: true},
				})
				return true
			})
			mu.Unlock()
		}
	}
}
