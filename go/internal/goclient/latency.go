package goclient

import (
	"context"
	"fmt"
	"github.com/zR-JB/graphite-meter/go/internal/route"
	"maps"
	"sync"
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

func (b wsBus) Close() { _ = b.conn.Close(websocket.StatusNormalClosure, "") }

func (r *runner) dialPingBus(ctx context.Context) (pingBus, error) {
	if r.latencyTarget.Transport == wire.TransportWebTransport {
		sess, err := wtDial(ctx, r.cfg, r.latencyTarget.Origin, route.WTPing, nil)
		if err != nil {
			return nil, err
		}
		return wtBus{sess: sess}, nil
	}
	u, err := wsEndpoint(r.latencyTarget.Origin, route.Ping)
	if err != nil {
		return nil, err
	}
	conn, response, err := websocket.Dial(ctx, u, &websocket.DialOptions{
		HTTPClient:      r.websocketHTTP,
		CompressionMode: websocket.CompressionDisabled,
	})
	if err != nil {
		if authErr := authResponseError(response); authErr != nil {
			return nil, authErr
		}
		return nil, err
	}
	return wsBus{conn: conn}, nil
}

func (r *runner) redialPingBus(ctx context.Context, deadline time.Time) (pingBus, error) {
	var bus pingBus
	err := restore(ctx, deadline, "latency channel", func(ctx context.Context) error {
		var err error
		bus, err = r.dialPingBus(ctx)
		return err
	})
	return bus, err
}

func (r *runner) measureLatency(
	ctx context.Context,
	stage Stage,
	underLoad bool,
	duration time.Duration,
	gate *stageGate,
) (result LatencyStats, failure error) {
	conn, err := r.dialPingBus(ctx)
	if err != nil {
		return LatencyStats{}, err
	}
	measureCtx, cancel := context.WithCancel(ctx)
	probes := &probeLedger{pending: map[uint32]time.Time{}, timeout: max(4*r.cfg.PingInterval, 250*time.Millisecond)}
	recvErr := make(chan error, 1)
	var readers sync.WaitGroup
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
	finish := func(err error) (LatencyStats, error) { return probes.finish(time.Now(), duration), err }
	emit := func(at time.Time, sample LatencySample) {
		sample.UnderLoad = underLoad
		r.emit(Event{Kind: EventLatency, At: at, Stage: stage, Latency: sample})
	}
	startReader := func(bus pingBus) {
		readers.Go(func() {
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
				if rtt, timedOut, ok := probes.reply(f.ID, now, f.HandlingNanos); ok {
					emit(now, LatencySample{RTT: rtt, TimedOut: timedOut})
				}
			}
		})
	}
	send := func() error {
		id, ok := probes.register(time.Now())
		if !ok {
			return nil
		}
		err := conn.Send(measureCtx, wire.EncodePing(id))
		if err != nil {
			probes.sendFailed(id)
		}
		return err
	}
	startReader(conn)
	if err := send(); err != nil {
		return finish(err)
	}
	gate.reportReady()
	start := gate.start
	var measured <-chan time.Time
	ticker := time.Tick(r.cfg.PingInterval)
	expiry := time.Tick(50 * time.Millisecond)
	for {
		select {
		case <-start:
			start = nil
			opened := gate.boundaryStart
			if opened.IsZero() {
				opened = time.Now()
			}
			probes.open(opened.Add(duration))
			timer := time.NewTimer(time.Until(opened.Add(duration)))
			defer timer.Stop()
			measured = timer.C
		case <-measureCtx.Done():
			return finish(measureCtx.Err())
		case <-measured:
			return finish(nil)
		case err := <-recvErr:
			if measureCtx.Err() != nil {
				return finish(measureCtx.Err())
			}
			if !probes.interrupt(time.Now()) {
				return finish(fmt.Errorf("latency channel failed: %w", err))
			}
			conn.Close()
			fresh, err := r.redialPingBus(measureCtx, probes.bound(time.Now().Add(redialWindow)))
			if err != nil {
				if measureCtx.Err() != nil {
					return finish(measureCtx.Err())
				}
				return finish(fmt.Errorf("latency channel failed: %w", err))
			}
			conn = fresh
			startReader(conn)
		case <-ticker:
			_ = send()
		case now := <-expiry:
			at, expired := probes.expire(now)
			for range expired {
				emit(at, LatencySample{TimedOut: true})
			}
		}
	}
}

// probeLedger.until is zero until the measured window opens.
type probeLedger struct {
	mu       sync.Mutex
	pending  map[uint32]time.Time
	nextID   uint32
	stats    latencyStats
	until    time.Time
	timeout  time.Duration
	answered bool
}

func (l *probeLedger) bound(t time.Time) time.Time {
	l.mu.Lock()
	defer l.mu.Unlock()
	return l.boundLocked(t)
}

func (l *probeLedger) boundLocked(t time.Time) time.Time {
	if !l.until.IsZero() && l.until.Before(t) {
		return l.until
	}
	return t
}

func (l *probeLedger) open(until time.Time) {
	l.mu.Lock()
	defer l.mu.Unlock()
	clear(l.pending)
	l.until = until
}

func (l *probeLedger) register(now time.Time) (uint32, bool) {
	l.mu.Lock()
	defer l.mu.Unlock()
	id := l.nextID
	l.nextID++
	if !l.until.IsZero() && !now.Before(l.until) {
		return 0, false
	}
	l.pending[id] = now
	return id, true
}

func (l *probeLedger) sendFailed(id uint32) {
	l.mu.Lock()
	defer l.mu.Unlock()
	if _, ok := l.pending[id]; ok {
		delete(l.pending, id)
		if !l.until.IsZero() {
			l.stats.sendFailures++
		}
	}
}

func (l *probeLedger) reply(id uint32, at time.Time, handlingNanos uint64) (time.Duration, bool, bool) {
	l.mu.Lock()
	defer l.mu.Unlock()
	sent, ok := l.pending[id]
	if !ok || !l.until.IsZero() && !at.Before(l.until) {
		return 0, false, false
	}
	l.answered = true
	delete(l.pending, id)
	if l.until.IsZero() {
		return 0, false, false
	}
	rtt := at.Sub(sent)
	timedOut := rtt >= l.timeout
	l.stats.add(rtt, timedOut, handlingNanos)
	return rtt, timedOut, true
}

func (l *probeLedger) expire(now time.Time) (time.Time, int) {
	l.mu.Lock()
	defer l.mu.Unlock()
	if l.until.IsZero() {
		return now, 0
	}
	now = l.boundLocked(now)
	expired := 0
	maps.DeleteFunc(l.pending, func(_ uint32, sent time.Time) bool {
		if now.Sub(sent) < l.timeout {
			return false
		}
		l.stats.add(0, true, 0)
		expired++
		return true
	})
	return now, expired
}

func (l *probeLedger) interrupt(now time.Time) (answered bool) {
	l.mu.Lock()
	defer l.mu.Unlock()
	if !l.answered {
		return false
	}
	if !l.until.IsZero() {
		l.stats.closePending(l.pending, l.boundLocked(now), l.timeout)
	}
	clear(l.pending)
	l.stats.breakContinuity()
	return true
}

func (l *probeLedger) finish(now time.Time, duration time.Duration) LatencyStats {
	l.mu.Lock()
	defer l.mu.Unlock()
	var elapsed time.Duration
	if !l.until.IsZero() {
		cutoff := l.boundLocked(now)
		elapsed = cutoff.Sub(l.until.Add(-duration))
		l.stats.closePending(l.pending, cutoff, l.timeout)
	}
	clear(l.pending)
	out := l.stats.snapshot()
	out.TimeoutAfter, out.Elapsed = l.timeout, elapsed
	return out
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
