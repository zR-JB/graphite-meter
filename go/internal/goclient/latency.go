package goclient

import (
	"context"
	"fmt"
	"github.com/zR-JB/graphite-meter/go/internal/route"
	"maps"
	"net/http"
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
	return dialLatencyBus(ctx, r.cfg, r.websocketHTTP, r.latencyTarget)
}

func dialLatencyBus(ctx context.Context, cfg Config, client *http.Client, target *wire.LatencyTarget) (pingBus, error) {
	if target.Transport == wire.TransportWebTransport {
		sess, err := wtDial(ctx, cfg, target.Origin, target.Routes.WTPing, nil)
		if err != nil {
			return nil, err
		}
		return wtBus{sess: sess}, nil
	}
	u, err := wsEndpoint(target.Origin, target.Routes.Ping)
	if err != nil {
		return nil, err
	}
	conn, response, err := websocket.Dial(ctx, u, &websocket.DialOptions{
		HTTPClient:      client,
		CompressionMode: websocket.CompressionDisabled,
	})
	if err != nil {
		if authErr := authResponseError(response); authErr != nil {
			return nil, authErr
		}
		return nil, fmt.Errorf("latency WebSocket connection failed: %w", err)
	}
	return wsBus{conn: conn}, nil
}

// verifyLatency proves the bus answers probe 0 and returns that warm round trip.
func verifyLatency(ctx context.Context, cfg Config, client *http.Client, target *wire.LatencyTarget) (time.Duration, error) {
	ctx, cancel := context.WithTimeout(ctx, 3*time.Second)
	defer cancel()
	bus, err := dialLatencyBus(ctx, cfg, client, target)
	if err != nil {
		return 0, err
	}
	defer bus.Close()
	datagrams := target.Transport == wire.TransportWebTransport
	for {
		sent := time.Now()
		if err := bus.Send(ctx, wire.EncodePing(0)); err != nil {
			return 0, fmt.Errorf("latency probe failed: %w", err)
		}
		for {
			replyCtx, cancelReply := ctx, context.CancelFunc(func() {})
			if datagrams {
				replyCtx, cancelReply = context.WithTimeout(ctx, 750*time.Millisecond)
			}
			reply, err := bus.Recv(replyCtx)
			cancelReply()
			if err != nil && (!datagrams || ctx.Err() != nil) {
				return 0, fmt.Errorf("latency readiness failed: %w", err)
			}
			if err != nil {
				break
			}
			if pong, err := wire.DecodePong(reply); err == nil && pong.ID == 0 {
				return time.Since(sent), nil
			}
		}
	}
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
	probes := &probeLedger{pending: map[uint32]probe{}, late: map[uint32]time.Time{}}
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
	var drain <-chan time.Time
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
			drain = timer.C
		case <-measureCtx.Done():
			return finish(measureCtx.Err())
		case <-drain:
			drain = nil
			if probes.drained(time.Now()) {
				return finish(nil)
			}
		case err := <-recvErr:
			switch {
			case measureCtx.Err() != nil:
				return finish(measureCtx.Err())
			case !probes.interrupt(time.Now()):
				return finish(fmt.Errorf("latency channel failed: %w", err))
			case probes.ended(time.Now()):
				return finish(nil)
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
			for _, at := range probes.expire(now) {
				emit(at, LatencySample{TimedOut: true})
			}
			if probes.ended(now) && probes.drained(now) {
				return finish(nil)
			}
		}
	}
}

const (
	probeTimeoutFloor = 250 * time.Millisecond
	probeTimeoutCeil  = 10 * time.Second
)

type probe struct {
	sent, deadline time.Time
	measured       bool
}

// probeLedger owns probes and the measured population; until is zero until the window opens.
// Each probe's deadline is fixed at send from an RFC 6298 estimate, so the cadence never sets it.
type probeLedger struct {
	mu           sync.Mutex
	pending      map[uint32]probe
	late         map[uint32]time.Time
	nextID       uint32
	stats        latencyStats
	until        time.Time
	srtt, rttvar time.Duration
	answered     bool
}

func (l *probeLedger) timeout() time.Duration {
	if !l.answered {
		return probeTimeoutFloor
	}
	return min(max(l.srtt+4*max(l.rttvar, time.Millisecond), probeTimeoutFloor), probeTimeoutCeil)
}

func (l *probeLedger) observe(rtt time.Duration) {
	if !l.answered {
		l.srtt, l.rttvar, l.answered = rtt, rtt/2, true
		return
	}
	l.rttvar = (3*l.rttvar + max(l.srtt-rtt, rtt-l.srtt)) / 4
	l.srtt = (7*l.srtt + rtt) / 8
}

func (l *probeLedger) bound(t time.Time) time.Time {
	l.mu.Lock()
	defer l.mu.Unlock()
	if !l.until.IsZero() && l.until.Before(t) {
		return l.until
	}
	return t
}

func (l *probeLedger) open(until time.Time) {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.until = until
}

func (l *probeLedger) ended(now time.Time) bool {
	l.mu.Lock()
	defer l.mu.Unlock()
	return !l.until.IsZero() && !now.Before(l.until)
}

// drained reports whether no measured probe still awaits its reply or deadline.
func (l *probeLedger) drained(now time.Time) bool {
	l.mu.Lock()
	defer l.mu.Unlock()
	if !now.Before(l.until.Add(probeTimeoutCeil)) {
		return true
	}
	for _, p := range l.pending {
		if p.measured {
			return false
		}
	}
	return true
}

func (l *probeLedger) register(now time.Time) (uint32, bool) {
	l.mu.Lock()
	defer l.mu.Unlock()
	id := l.nextID
	l.nextID++
	if !l.until.IsZero() && !now.Before(l.until) {
		return 0, false
	}
	l.pending[id] = probe{sent: now, deadline: now.Add(l.timeout()), measured: !l.until.IsZero()}
	return id, true
}

func (l *probeLedger) sendFailed(id uint32) {
	l.mu.Lock()
	defer l.mu.Unlock()
	if p, ok := l.pending[id]; ok {
		delete(l.pending, id)
		if p.measured {
			l.stats.sendFailures++
		}
	}
}

func (l *probeLedger) reply(id uint32, at time.Time, handlingNanos uint64) (time.Duration, bool, bool) {
	l.mu.Lock()
	defer l.mu.Unlock()
	if sent, ok := l.late[id]; ok {
		delete(l.late, id)
		l.observe(at.Sub(sent))
		return 0, false, false
	}
	p, ok := l.pending[id]
	if !ok {
		return 0, false, false
	}
	delete(l.pending, id)
	rtt := at.Sub(p.sent)
	l.observe(rtt)
	if !p.measured {
		return 0, false, false
	}
	timedOut := !at.Before(p.deadline)
	l.stats.add(rtt, timedOut, handlingNanos)
	return rtt, timedOut, true
}

// expire resolves probes past their deadline; a late reply still teaches the estimator.
func (l *probeLedger) expire(now time.Time) []time.Time {
	l.mu.Lock()
	defer l.mu.Unlock()
	var expired []time.Time
	for id, p := range l.pending {
		if now.Before(p.deadline) {
			continue
		}
		delete(l.pending, id)
		l.late[id] = p.sent
		if p.measured {
			l.stats.add(0, true, 0)
			expired = append(expired, p.deadline)
		}
	}
	maps.DeleteFunc(l.late, func(_ uint32, sent time.Time) bool { return now.Sub(sent) > probeTimeoutCeil })
	return expired
}

func (l *probeLedger) interrupt(now time.Time) (answered bool) {
	l.mu.Lock()
	defer l.mu.Unlock()
	if !l.answered {
		return false
	}
	l.closePending(now)
	return true
}

func (l *probeLedger) finish(now time.Time, duration time.Duration) LatencyStats {
	l.mu.Lock()
	defer l.mu.Unlock()
	var elapsed time.Duration
	if !l.until.IsZero() {
		cutoff := now
		if l.until.Before(now) {
			cutoff = l.until
		}
		elapsed = cutoff.Sub(l.until.Add(-duration))
		l.closePending(now)
	}
	out := l.stats.snapshot()
	out.Elapsed = elapsed
	return out
}

// closePending separates known deadline expirations from probes interrupted before their deadline.
func (l *probeLedger) closePending(now time.Time) {
	for _, p := range l.pending {
		switch {
		case !p.measured:
		case !now.Before(p.deadline):
			l.stats.timeouts++
		default:
			l.stats.unresolved++
		}
	}
	clear(l.pending)
	l.stats.breakContinuity()
}
