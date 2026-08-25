//go:build stress && unix

// The CPU column reads getrusage, so this harness is Unix-only; `just stress`
// is a measurement tool, never part of ci.

package server

import (
	"context"
	"fmt"
	"runtime"
	"slices"
	"strings"
	"sync"
	"sync/atomic"
	"syscall"
	"testing"
	"time"

	"github.com/coder/websocket"
	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// TestSaturationEnvelope is the repeatable loopback stress harness behind
// `just stress`. It boots one server and measures what an idle-cadence
// observer's RTT does while N loader clients saturate the transfer paths, over
// kernel TCP (fetch) and userspace QUIC (WebTransport), plus a CPU-constrained
// pass. Server and clients share the process, so the CPU column is the whole
// measurement stack; on loopback that is exactly the contention under study.
func TestSaturationEnvelope(t *testing.T) {
	// The harness studies measurement contamination, not admission refusal, and
	// every client shares 127.0.0.1: lift the caps out of the way.
	// Every cap, not merely most: MaxSessionsPerClient left at its default of 16
	// while every loopback loader shares one client key made wt-load-8-redialing
	// -- which churns a session per loader against a 5 s bound -- a measurement
	// of admission refusal with 8 slots of slack rather than one of contention.
	liftCaps := func(c *config.Config) {
		c.MaxActiveMeasurements, c.MaxActiveMeasurementsPerClient = 4096, 4096
		c.MaxActiveSessions, c.MaxSessionsPerClient = 4096, 4096
		c.MaxConnections, c.MaxConnectionsPerClient = 8192, 8192
	}
	h3Base, base := wtTestOrigins(t, liftCaps)

	t.Logf("GOMAXPROCS=%d", runtime.GOMAXPROCS(0))
	t.Log("loaders alternate download/upload (even index down, 2 forced lanes each); spammers are reply-driven ping chains")
	t.Logf("%-28s %8s %8s %8s %6s %8s %8s %10s %6s", "scenario", "p50", "p95", "p99", "loss", "down", "up", "pings/s", "cpu")

	// A second server whose sessions die every few seconds, so one scenario
	// drives the redial and progress-handover paths under the same contention
	// the harness measures rather than against an idle server.
	_, redialBase := wtTestOrigins(t, func(c *config.Config) {
		liftCaps(c)
		// The session bound may not sit below the request bound, so both drop.
		c.MaxOperationDuration = 5 * time.Second
		c.MaxSessionDuration = 5 * time.Second
	})

	run := func(name string, mix loadMix) {
		if mix.procs > 0 {
			old := runtime.GOMAXPROCS(mix.procs)
			defer runtime.GOMAXPROCS(old)
		}
		ctx, cancel := context.WithCancel(t.Context())
		defer cancel()
		loadBase := base
		if mix.base != "" {
			loadBase = mix.base
		}
		// A loader that gave up leaves the row reading as a measured result on a
		// server under load, when in truth nothing was loading it. The envelope
		// in docs/BENCHMARKS.md is taken from these rows, so a dead loader has
		// to be visible rather than silent.
		var downBytes, upBytes, spamPings, loaderExits atomic.Uint64
		var wg sync.WaitGroup
		for i := range mix.loaders {
			upload := i%2 == 1
			bytes := &downBytes
			if upload {
				bytes = &upBytes
			}
			wg.Go(func() {
				loaderRun(ctx, loadBase, mix.transport, upload, bytes, &loaderExits)
			})
		}
		for range mix.spammers {
			wg.Go(func() {
				var err error
				if mix.spamWT {
					err = wtPingSpam(ctx, h3Base, &spamPings)
				} else {
					err = wsPingSpam(ctx, "ws"+strings.TrimPrefix(base, "http")+routePing, &spamPings)
				}
				if err != nil && ctx.Err() == nil {
					loaderExits.Add(1)
				}
			})
		}
		// Let the loaders' congestion windows ramp before observing. The two
		// observers run inside the same sustained load window.
		time.Sleep(1500 * time.Millisecond)
		for _, bus := range []struct{ transport, label string }{{"websocket", "ws"}, {"webtransport", "wt"}} {
			cpu0, wall0 := cpuNow()
			down0, up0 := downBytes.Load(), upBytes.Load()
			pings0 := spamPings.Load()
			rtts, loss := observe(t, base, bus.transport)
			cpu1, wall1 := cpuNow()
			window := wall1.Sub(wall0).Seconds()
			gbps := func(delta uint64) float64 { return float64(delta) * 8 / window / 1e9 }
			spamRate := float64(spamPings.Load()-pings0) / window
			if len(rtts) == 0 {
				t.Errorf("%s/%s: observer collected no samples", name, bus.label)
				continue
			}
			// Without this the row prints as a measurement taken under load
			// when the load had in fact given up, and that number is what
			// docs/BENCHMARKS.md publishes.
			if exits := loaderExits.Load(); exits > 0 {
				t.Errorf("%s/%s: %d loader(s) stopped early, so this row was not measured under the load it names", name, bus.label, exits)
			}
			t.Logf("%-28s %8s %8s %8s %5.1f%% %5.1f Gb %5.1f Gb %9.0f/s %5.0f%%",
				name+"/"+bus.label, pct(rtts, 50), pct(rtts, 95), pct(rtts, 99), loss*100,
				gbps(downBytes.Load()-down0), gbps(upBytes.Load()-up0), spamRate,
				(cpu1-cpu0).Seconds()/window*100)
		}
		cancel()
		wg.Wait()
	}

	run("idle", loadMix{})
	for _, n := range []int{2, 4, 8, 16, 32} {
		run(fmt.Sprintf("fetch-load-%d", n), loadMix{loaders: n, transport: "fetch-stream"})
	}
	run("wt-load-8", loadMix{loaders: 8, transport: "webtransport"})
	// Same load against the short-bound server: goodput here is what survives
	// a session kill every 5 s.
	run("wt-load-8-redialing", loadMix{loaders: 8, transport: "webtransport", base: redialBase})
	run("ws-spam-16", loadMix{spammers: 16})
	run("ws-spam-64", loadMix{spammers: 64})
	run("wt-spam-16", loadMix{spammers: 16, spamWT: true})
	run("wt-spam-64", loadMix{spammers: 64, spamWT: true})
	run("fetch-load-8+ws-spam-32", loadMix{loaders: 8, transport: "fetch-stream", spammers: 32})
	run("fetch-load-8-2cores", loadMix{loaders: 8, transport: "fetch-stream", procs: 2})
}

// loadMix is one scenario's background load: bulk transfer loaders, reply-driven
// ping spammers, and an optional GOMAXPROCS ceiling.
type loadMix struct {
	loaders   int
	transport string
	spammers  int
	spamWT    bool
	procs     int
	// base overrides the server the loaders drive; empty uses the main one.
	base string
}

// observe runs one latency-only client over the named bus and returns its raw
// RTT samples and loss ratio.
func observe(t *testing.T, base, bus string) ([]time.Duration, float64) {
	t.Helper()
	cfg := goclient.DefaultConfig()
	cfg.BaseURL = base
	cfg.InsecureSkipTLSVerify = true
	cfg.LatencyTransport = bus
	cfg.Stages = goclient.StageSet{Latency: true}
	cfg.Warmup = 200 * time.Millisecond
	cfg.LatencyDuration = 6 * time.Second
	cfg.PingInterval = 20 * time.Millisecond
	var rtts []time.Duration
	var lost, total int
	ctx, cancel := context.WithTimeout(t.Context(), 30*time.Second)
	defer cancel()
	err := goclient.Run(ctx, cfg, func(e goclient.Event) {
		if e.Kind != goclient.EventLatency {
			return
		}
		total++
		if e.Latency.Lost {
			lost++
			return
		}
		rtts = append(rtts, e.Latency.RTT)
	})
	if err != nil {
		t.Errorf("observer: %v", err)
	}
	if total == 0 {
		return nil, 0
	}
	return rtts, float64(lost) / float64(total)
}

// loaderRun drives one continuous transfer client until ctx ends, counting the
// bytes it moves. Odd loaders upload, even ones download.
func loaderRun(ctx context.Context, base, transport string, upload bool, bytes, exits *atomic.Uint64) {
	cfg := goclient.DefaultConfig()
	cfg.BaseURL = base
	cfg.ThroughputTransport = transport
	cfg.InsecureSkipTLSVerify = true
	cfg.Stages = goclient.StageSet{Download: !upload, Upload: upload}
	cfg.Warmup = 100 * time.Millisecond
	cfg.DownloadDuration = time.Hour
	cfg.UploadDuration = time.Hour
	cfg.TransferStreams = goclient.TransferStreamPolicy{Forced: 2}
	cfg.LoadedLatency = false
	var last uint64
	err := goclient.Run(ctx, cfg, func(e goclient.Event) {
		if e.Kind == goclient.EventThroughput {
			if n := e.Throughput.TotalBytes; n > last {
				bytes.Add(n - last)
				last = n
			}
		}
	})
	// The scenario's own context ending is the clean stop.
	if err != nil && ctx.Err() == nil {
		exits.Add(1)
	}
}

// wsPingSpam runs one reply-driven chain over a WebSocket: a new PING the
// moment the PONG lands, the heaviest per-client cadence the bus allows.
func wsPingSpam(ctx context.Context, url string, pings *atomic.Uint64) error {
	conn, _, err := websocket.Dial(ctx, url, nil)
	if err != nil {
		return err
	}
	defer conn.CloseNow()
	var id uint32
	send := func() error {
		id++
		return conn.Write(ctx, websocket.MessageText, []byte(wire.Encode(wire.Frame{Op: wire.OpPING, ID: id})))
	}
	if err := send(); err != nil {
		return err
	}
	for ctx.Err() == nil {
		_, msg, err := conn.Read(ctx)
		if err != nil {
			return nil
		}
		if f, err := wire.Decode(string(msg)); err != nil || f.Op != wire.OpPONG {
			continue
		}
		pings.Add(1)
		if err := send(); err != nil {
			return err
		}
	}
	return nil
}

// wtPingSpam is the same chain over session datagrams.
func wtPingSpam(ctx context.Context, origin string, pings *atomic.Uint64) error {
	wtTransport := insecureWTTransport()
	defer wtTransport.Close()
	_, sess, err := wtTransport.Dial(ctx, origin+routeWTPing, nil)
	if err != nil {
		return err
	}
	defer sess.CloseWithError(0, "")
	var id uint32
	send := func() error {
		id++
		return sess.SendDatagram([]byte(wire.Encode(wire.Frame{Op: wire.OpPING, ID: id})))
	}
	if err := send(); err != nil {
		return err
	}
	for ctx.Err() == nil {
		msg, err := sess.ReceiveDatagram(ctx)
		if err != nil {
			return nil
		}
		if f, err := wire.Decode(string(msg)); err != nil || f.Op != wire.OpPONG {
			continue
		}
		pings.Add(1)
		if err := send(); err != nil {
			return err
		}
	}
	return nil
}

func pct(rtts []time.Duration, p int) time.Duration {
	s := slices.Clone(rtts)
	slices.Sort(s)
	i := min(len(s)-1, len(s)*p/100)
	return s[i]
}

// cpuNow reads the process's cumulative CPU time and the wall clock.
func cpuNow() (time.Duration, time.Time) {
	var ru syscall.Rusage
	_ = syscall.Getrusage(syscall.RUSAGE_SELF, &ru)
	cpu := time.Duration(ru.Utime.Nano() + ru.Stime.Nano())
	return cpu, time.Now()
}
