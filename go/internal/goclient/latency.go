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

// pingBus is the message channel the ping chain runs over. Loss on an
// unreliable bus is physical packet loss; on a reliable one it is a stall.
type pingBus interface {
	Send(ctx context.Context, msg string) error
	Recv(ctx context.Context) (string, error)
	Close()
}

// wsBus carries the wire protocol as WebSocket text frames.
type wsBus struct{ conn *websocket.Conn }

func (b wsBus) Send(ctx context.Context, msg string) error {
	return b.conn.Write(ctx, websocket.MessageText, []byte(msg))
}

func (b wsBus) Recv(ctx context.Context) (string, error) {
	_, msg, err := b.conn.Read(ctx)
	return string(msg), err
}

func (b wsBus) Close() { b.conn.Close(websocket.StatusNormalClosure, "") } //nolint:errcheck // the samples are already collected

// dialPingBus opens the latency channel over the target's advertised transport.
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

// redialPingBus re-opens the latency channel after the bus dropped or outlived
// its route's lifetime bound, retrying with backoff until ctx ends or the
// measurement window closes at deadline.
func (r *runner) redialPingBus(ctx context.Context, deadline time.Time) (pingBus, string, error) {
	ctx, cancel := context.WithDeadline(ctx, deadline)
	defer cancel()
	for {
		dialCtx, dialCancel := context.WithTimeout(ctx, 3*time.Second)
		bus, proto, err := r.dialPingBus(dialCtx)
		dialCancel()
		if err == nil {
			return bus, proto, nil
		}
		select {
		case <-ctx.Done():
			return nil, proto, ctx.Err()
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
	// The deferred receiver must be the LATEST bus, not the first dial's.
	defer func() { conn.Close() }()
	// A failed hello needs no handling here: the read goroutine below sees the
	// same broken channel and reports it through recvErr.
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
	// When the measured window closes; zero until it opens. It bounds a
	// mid-stage bus redial, which nothing else can interrupt.
	var windowEnd time.Time
	// The read goroutine outlives every return below, so stats must be
	// snapshotted under mu.
	snapshot := func() LatencyStats {
		mu.Lock()
		defer mu.Unlock()
		return stats.snapshot()
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
		return conn.Send(measureCtx, wire.Encode(wire.Frame{Op: wire.OpPING, ID: id}))
	}
	if err := send(); err != nil {
		return LatencyStats{}, err
	}
	lossAfter := max(4*r.cfg.PingInterval, 250*time.Millisecond)
	for {
		select {
		case <-start:
			// A closed gate stays ready, so drop it once armed rather than
			// spinning this select for the whole window.
			start = nil
			mu.Lock()
			pending = make(map[uint32]time.Time)
			mu.Unlock()
			measuring.Store(true)
			timer := time.NewTimer(duration)
			defer timer.Stop()
			measureTimer = timer.C
			windowEnd = time.Now().Add(duration)
		case <-measureCtx.Done():
			// BYE releases the server's session promptly; the samples are
			// already collected, so a failed farewell changes nothing.
			_ = conn.Send(context.Background(), wire.Encode(wire.Frame{Op: wire.OpBYE}))
			return snapshot(), nil
		case <-measureTimer:
			_ = conn.Send(context.Background(), wire.Encode(wire.Frame{Op: wire.OpBYE}))
			return snapshot(), nil
		case err := <-recvErr:
			if measureCtx.Err() != nil {
				return snapshot(), nil
			}
			// A bus that dies before ever answering never established; only a
			// proven bus reconnects.
			if !everPong.Load() {
				return LatencyStats{}, fmt.Errorf("latency channel failed: %w", err)
			}
			// The bus outlived its route's lifetime bound or dropped: reconnect
			// and continue, as the browser worker does. The gap's pings clear
			// without counting loss, since a connection gap is not packet loss.
			conn.Close()
			// The redial is bounded: a server that stays down must end the
			// stage with the samples already collected rather than outlive its
			// own window, which nothing else can service from in here.
			budget := windowEnd
			if budget.IsZero() {
				budget = time.Now().Add(duration)
			}
			fresh, freshProto, dialErr := r.redialPingBus(measureCtx, budget)
			if dialErr != nil {
				if measureCtx.Err() != nil || !time.Now().Before(budget) {
					return snapshot(), nil
				}
				return LatencyStats{}, fmt.Errorf("latency channel failed: %w", dialErr)
			}
			conn, proto = fresh, freshProto
			mu.Lock()
			pending = make(map[uint32]time.Time)
			mu.Unlock()
			_ = conn.Send(measureCtx, wire.Encode(wire.Frame{Op: wire.OpHI, Proto: proto}))
			go readLoop(conn)
		case <-ticker.C:
			// A dead bus surfaces through recvErr and reconnects; a skipped ping
			// costs one sample, not the stage.
			_ = send()
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
