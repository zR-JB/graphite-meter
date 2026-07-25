//go:build stress

package server

import (
	"context"
	"fmt"
	"net/http"
	"runtime"
	"sort"
	"sync"
	"sync/atomic"
	"syscall"
	"testing"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
)

// TestSaturationEnvelope is the repeatable loopback stress harness behind
// `just stress`. It boots one server and measures what an idle-cadence
// observer's RTT does while N loader clients saturate the transfer paths, over
// kernel TCP (fetch) and userspace QUIC (WebTransport), plus a CPU-constrained
// pass. Server and clients share the process, so the CPU column is the whole
// measurement stack; on loopback that is exactly the contention under study.
func TestSaturationEnvelope(t *testing.T) {
	cert, key := writeCertificate(t, t.TempDir(), "srv", "127.0.0.1",
		time.Now().Add(-time.Hour), time.Now().Add(time.Hour))
	cfg := config.Default()
	cfg.Native.H1 = freeTCPAddr(t)
	cfg.Native.H3 = freeTCPAddr(t)
	cfg.TLSCert, cfg.TLSKey = cert, key
	// The harness studies measurement contamination, not admission refusal, and
	// every client shares 127.0.0.1: lift the caps out of the way.
	cfg.MaxActiveMeasurements = 4096
	cfg.MaxActiveMeasurementsPerClient = 4096
	cfg.MaxConnections = 8192
	cfg.MaxConnectionsPerClient = 8192
	defer runUntilCancel(t, &cfg)()
	waitForOK(t, http.DefaultClient, "http://"+cfg.Native.H1+"/preflight")
	base := "http://" + cfg.Native.H1

	t.Logf("GOMAXPROCS=%d", runtime.GOMAXPROCS(0))
	t.Logf("%-28s %8s %8s %8s %6s %10s %6s", "scenario", "p50", "p95", "p99", "loss", "goodput", "cpu")

	run := func(name string, loaders int, loaderTransport string, procs int) {
		if procs > 0 {
			old := runtime.GOMAXPROCS(procs)
			defer runtime.GOMAXPROCS(old)
		}
		ctx, cancel := context.WithCancel(context.Background())
		defer cancel()
		var loaderBytes atomic.Uint64
		var wg sync.WaitGroup
		for i := range loaders {
			wg.Add(1)
			go func() {
				defer wg.Done()
				loaderRun(ctx, base, loaderTransport, i%2 == 1, &loaderBytes)
			}()
		}
		// Let the loaders' congestion windows ramp before observing. The two
		// observers run inside the same sustained load window.
		time.Sleep(1500 * time.Millisecond)
		for _, bus := range []struct{ transport, label string }{{"websocket", "ws"}, {"webtransport", "wt"}} {
			cpu0, wall0 := cpuNow()
			bytes0 := loaderBytes.Load()
			rtts, loss := observe(t, base, bus.transport)
			cpu1, wall1 := cpuNow()
			goodput := float64(loaderBytes.Load()-bytes0) * 8 / wall1.Sub(wall0).Seconds() / 1e9
			if len(rtts) == 0 {
				t.Errorf("%s/%s: observer collected no samples", name, bus.label)
				continue
			}
			t.Logf("%-28s %8s %8s %8s %5.1f%% %7.2f Gb %5.0f%%",
				name+"/"+bus.label, pct(rtts, 50), pct(rtts, 95), pct(rtts, 99), loss*100, goodput,
				(cpu1-cpu0).Seconds()/wall1.Sub(wall0).Seconds()*100)
		}
		cancel()
		wg.Wait()
	}

	run("idle", 0, "", 0)
	for _, n := range []int{2, 4, 8, 16, 32} {
		run(fmt.Sprintf("fetch-load-%d", n), n, "fetch-stream", 0)
	}
	run("wt-load-8", 8, "webtransport", 0)
	run("fetch-load-8-2cores", 8, "fetch-stream", 2)
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
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
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
func loaderRun(ctx context.Context, base, transport string, upload bool, bytes *atomic.Uint64) {
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
	_ = goclient.Run(ctx, cfg, func(e goclient.Event) {
		if e.Kind == goclient.EventThroughput {
			if n := e.Throughput.TotalBytes; n > last {
				bytes.Add(n - last)
				last = n
			}
		}
	})
}

func pct(rtts []time.Duration, p int) time.Duration {
	s := append([]time.Duration(nil), rtts...)
	sort.Slice(s, func(i, j int) bool { return s[i] < s[j] })
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
