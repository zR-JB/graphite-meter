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

func (r *runner) dialPingBus(ctx context.Context) (pingBus, error) {
	if r.latencyTarget.Transport == wire.TransportWebTransport {
		sess, err := wtDial(ctx, r.cfg, r.latencyTarget.Origin, r.latencyTarget.Routes.WTPing, nil)
		if err != nil {
			return nil, err
		}
		return wtBus{sess: sess}, nil
	}
	u, err := wsEndpoint(r.latencyTarget.Origin, r.latencyTarget.Routes.Ping)
	if err != nil {
		return nil, err
	}
	conn, response, err := websocket.Dial(ctx, u, &websocket.DialOptions{HTTPClient: r.websocketHTTP, CompressionMode: websocket.CompressionDisabled})
	if err != nil {
		if authErr := authResponseError(response); authErr != nil {
			return nil, authErr
		}
		return nil, err
	}
	return wsBus{conn: conn}, nil
}

const busRedialWindow = 2 * time.Second

func (r *runner) redialPingBus(ctx context.Context, deadline time.Time) (pingBus, error) {
	redialCtx, cancel := context.WithDeadline(ctx, deadline)
	defer cancel()
	var lastErr error
	for {
		dialCtx, dialCancel := context.WithTimeout(redialCtx, 3*time.Second)
		bus, err := r.dialPingBus(dialCtx)
		dialCancel()
		if err == nil {
			return bus, nil
		}
		if _, authRequired := errors.AsType[*AuthRequiredError](err); authRequired {
			return nil, err
		}
		if !errors.Is(err, context.DeadlineExceeded) || lastErr == nil {
			lastErr = err
		}
		select {
		case <-redialCtx.Done():
			if ctx.Err() != nil {
				return nil, ctx.Err()
			}
			if lastErr != nil {
				return nil, fmt.Errorf("latency channel not reconnected within %v: %w", busRedialWindow, lastErr)
			}
			return nil, redialCtx.Err()
		case <-time.After(wtRedialBackoff):
		}
	}
}

func (r *runner) measureLatency(ctx context.Context, stage string, underLoad bool, duration time.Duration, gate *stageGate) (result LatencyStats, failure error) {
	if r.latencyTarget == nil {
		return LatencyStats{}, fmt.Errorf("no latency target selected")
	}
	conn, err := r.dialPingBus(ctx)
	if err != nil {
		return LatencyStats{}, err
	}

	measureCtx, cancel := context.WithCancel(ctx)

	pending := make(map[uint32]time.Time)
	var mu sync.Mutex // guards pending and stats
	var nextID uint32
	stats := latencyStats{}
	timeoutAfter := max(4*r.cfg.PingInterval, 250*time.Millisecond)
	recvErr := make(chan error, 1)
	var everPong atomic.Bool
	var measuring atomic.Bool
	var readers sync.WaitGroup
	var measureTimer <-chan time.Time
	var measuredUntil time.Time // guarded by mu, like pending and stats
	defer func() {
		cancel()
		readers.Wait()
		conn.Close()
	}()
	defer func() {
		if failure != nil {
			gate.cancel(failure)
		}
	}()
	finish := func(failure error) (LatencyStats, error) {
		mu.Lock()
		var elapsed time.Duration
		if measuring.Load() {
			cutoff := time.Now()
			if measuredUntil.Before(cutoff) {
				cutoff = measuredUntil
			}
			elapsed = cutoff.Sub(measuredUntil.Add(-duration))
			stats.closePending(pending, cutoff, timeoutAfter)
		}
		measuring.Store(false)
		clear(pending)
		out := stats.snapshot()
		out.TimeoutAfter, out.Elapsed = timeoutAfter, elapsed
		mu.Unlock()
		return out, failure
	}

	readLoop := func(bus pingBus) {
		for {
			msg, err := bus.Recv(measureCtx)
			if err != nil {
				recvErr <- err
				return
			}
			now := time.Now() // Reply receipt ends raw RTT before diagnostic parsing.
			f, err := wire.DecodePong(msg)
			if err != nil {
				continue
			}
			mu.Lock()
			sent, ok := pending[f.ID]
			if measuring.Load() && !measuredUntil.IsZero() && !now.Before(measuredUntil) {
				mu.Unlock()
				continue
			}
			if ok {
				everPong.Store(true)
				delete(pending, f.ID)
			}
			if !ok || !measuring.Load() {
				mu.Unlock()
				continue
			}
			rtt := now.Sub(sent)
			timedOut := rtt >= timeoutAfter
			handling := stats.add(rtt, timedOut, f.HandlingNanos)
			mu.Unlock()
			r.emit(Event{
				Kind:    EventLatency,
				At:      now,
				Stage:   stage,
				Latency: LatencySample{Stage: stage, RTT: rtt, UnderLoad: underLoad, TimedOut: timedOut, ReflectorHandling: handling},
			})
		}
	}
	startReader := func(bus pingBus) { readers.Go(func() { readLoop(bus) }) }
	startReader(conn)

	ticker := time.Tick(r.cfg.PingInterval)
	timeoutTicker := time.Tick(50 * time.Millisecond)
	send := func() error {
		mu.Lock()
		id := nextID
		nextID++
		now := time.Now()
		if measuring.Load() && !now.Before(measuredUntil) {
			mu.Unlock()
			return nil
		}
		pending[id] = now
		mu.Unlock()
		err := conn.Send(measureCtx, wire.EncodePing(id))
		if err != nil {
			mu.Lock()
			if _, ok := pending[id]; ok {
				delete(pending, id)
				if measuring.Load() {
					stats.sendFailures++
				}
			}
			mu.Unlock()
		}
		return err
	}
	if err := send(); err != nil {
		return finish(err)
	}
	gate.markReady()
	start := gate.start
	for {
		select {
		case <-start:
			start = nil
			mu.Lock()
			clear(pending)
			measuring.Store(true)
			if gate.boundaryStart.IsZero() {
				measuredUntil = time.Now().Add(duration)
			} else {
				measuredUntil = gate.boundaryStart.Add(duration)
			}
			mu.Unlock()
			timer := time.NewTimer(max(0, time.Until(measuredUntil)))
			defer timer.Stop()
			measureTimer = timer.C
		case <-measureCtx.Done():
			return finish(measureCtx.Err())
		case <-measureTimer:
			return finish(nil)
		case err := <-recvErr:
			if measureCtx.Err() != nil {
				return finish(measureCtx.Err())
			}
			if !everPong.Load() {
				return finish(fmt.Errorf("latency channel failed: %w", err))
			}
			mu.Lock()
			if measuring.Load() {
				at := time.Now()
				if measuredUntil.Before(at) {
					at = measuredUntil
				}
				stats.closePending(pending, at, timeoutAfter)
			}
			clear(pending)
			stats.breakContinuity()
			mu.Unlock()
			conn.Close()
			redialDeadline := time.Now().Add(busRedialWindow)
			if measuring.Load() && measuredUntil.Before(redialDeadline) {
				redialDeadline = measuredUntil
			}
			fresh, dialErr := r.redialPingBus(measureCtx, redialDeadline)
			if dialErr != nil {
				if measureCtx.Err() != nil {
					return finish(measureCtx.Err())
				}
				return finish(fmt.Errorf("latency channel failed: %w", dialErr))
			}
			conn = fresh
			mu.Lock()
			clear(pending)
			mu.Unlock()
			startReader(conn)
		case <-ticker:
			_ = send()
		case now := <-timeoutTicker:
			if !measuring.Load() {
				continue
			}
			mu.Lock()
			if !measuredUntil.IsZero() && measuredUntil.Before(now) {
				now = measuredUntil
			}
			maps.DeleteFunc(pending, func(_ uint32, sent time.Time) bool {
				if now.Sub(sent) < timeoutAfter {
					return false
				}
				stats.add(0, true, 0)
				r.emit(Event{
					Kind:    EventLatency,
					At:      now,
					Stage:   stage,
					Latency: LatencySample{Stage: stage, UnderLoad: underLoad, TimedOut: true},
				})
				return true
			})
			mu.Unlock()
		}
	}
}

// closePending separates known deadline expirations from probes interrupted before their deadline.
func (s *latencyStats) closePending(pending map[uint32]time.Time, cutoff time.Time, timeout time.Duration) {
	for _, sent := range pending {
		if cutoff.Sub(sent) >= timeout {
			s.timeouts++
		} else {
			s.unresolved++
		}
	}
	clear(pending)
	s.breakContinuity()
}
