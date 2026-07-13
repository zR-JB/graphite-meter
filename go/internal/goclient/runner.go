package goclient

import (
	"context"
	"crypto/tls"
	"errors"
	"fmt"
	"net"
	"net/http"
	"net/url"
	"sync"
	"time"

	"github.com/quic-go/quic-go/http3"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func Run(ctx context.Context, cfg Config, emit func(Event)) error {
	cfg = cfg.normalized()
	baseTransport := func() *http.Transport {
		return &http.Transport{
			Proxy:                 http.ProxyFromEnvironment,
			DialContext:           (&net.Dialer{Timeout: 10 * time.Second, KeepAlive: 30 * time.Second}).DialContext,
			ForceAttemptHTTP2:     false,
			MaxIdleConns:          cfg.MaxIdleConnsPerHost * 2,
			MaxIdleConnsPerHost:   cfg.MaxIdleConnsPerHost,
			MaxConnsPerHost:       0,
			IdleConnTimeout:       90 * time.Second,
			ResponseHeaderTimeout: cfg.ResponseHeaderTimeout,
			ExpectContinueTimeout: cfg.ExpectContinueTimeout,
			TLSClientConfig:       &tls.Config{InsecureSkipVerify: cfg.InsecureSkipTLSVerify}, //nolint:gosec
			// 256 KiB socket buffers (vs the 4 KiB default) so a saturated link moves
			// each direction in large reads/writes instead of thousands of tiny
			// syscalls — the upload write side felt this most (see uploadLane).
			WriteBufferSize: 256 * 1024,
			ReadBufferSize:  256 * 1024,
		}
	}
	discoveryTransport := baseTransport()
	defer discoveryTransport.CloseIdleConnections()
	discoveryClient := &http.Client{Transport: discoveryTransport}

	pf, err := getPreflight(ctx, discoveryClient, cfg.BaseURL)
	if err != nil {
		emit(Event{Kind: EventError, At: time.Now(), Err: err})
		return err
	}
	target, protocol, err := selectTarget(cfg, pf)
	if err != nil {
		emit(Event{Kind: EventError, At: time.Now(), Err: err})
		return err
	}
	transfer, closeTransfer := protocolClient(cfg, protocol, baseTransport)
	defer closeTransfer()
	probe, err := getProbe(ctx, transfer, target)
	if err != nil {
		emit(Event{Kind: EventError, At: time.Now(), Err: err})
		return err
	}
	expected := map[string]string{"http1": "http/1.1", "http2": "h2", "http3": "h3"}[protocol]
	if probe.ProtocolNegotiated != expected {
		return fmt.Errorf("selected %s but probe negotiated %s", protocol, probe.ProtocolNegotiated)
	}
	emit(Event{Kind: EventPreflight, At: time.Now(), Preflight: &pf, Probe: &probe, Message: protocol})

	wsTransport := baseTransport()
	wsp := &http.Protocols{}
	wsp.SetHTTP1(true)
	wsTransport.Protocols = wsp
	defer wsTransport.CloseIdleConnections()
	r := runner{
		cfg: cfg, streams: cfg.TransferStreams.Resolve(protocol),
		http: transfer, websocketHTTP: &http.Client{Transport: wsTransport},
		target: target, preflight: pf, probe: probe, emit: emit,
	}
	if cfg.Stages.Latency {
		if err := r.runLatencyStage(ctx, "latency", false, cfg.LatencyDuration); err != nil {
			return r.fail(err)
		}
	}
	if cfg.Stages.Download {
		if err := r.runTransferStage(ctx, "download", []Direction{Down}, cfg.DownloadDuration); err != nil {
			return r.fail(err)
		}
	}
	if cfg.Stages.Upload {
		if err := r.runTransferStage(ctx, "upload", []Direction{Up}, cfg.UploadDuration); err != nil {
			return r.fail(err)
		}
	}
	if cfg.Stages.Bidirectional {
		if err := r.runTransferStage(ctx, "bidirectional", []Direction{Down, Up}, cfg.BidirectionalDuration); err != nil {
			return r.fail(err)
		}
	}
	emit(Event{Kind: EventComplete, At: time.Now(), Message: "complete"})
	return nil
}

type runner struct {
	cfg           Config
	streams       int
	http          *http.Client
	websocketHTTP *http.Client
	target        *wire.Target
	preflight     wire.Preflight
	probe         wire.Probe
	emit          func(Event)
	// Idle RTT captured from the latency stage; used to stretch later stages'
	// warmup so TCP slow-start fills the BDP before measuring (0 until measured).
	idleRTT time.Duration
}

// laneStagger spreads lane starts so their congestion windows don't ramp in
// lockstep (synchronised overshoot → synchronised loss/backoff).
const laneStagger = 75 * time.Millisecond

// adaptiveWarmup stretches a stage's warmup to ~10 RTTs (the configured value as
// floor, capped) so slow-start finishes before the measured window opens. rtt <= 0
// (latency stage not yet run / disabled) ⇒ the configured value.
func adaptiveWarmup(base, rtt time.Duration) time.Duration {
	const slowStartRTTs = 10
	const ceil = 4 * time.Second
	w := slowStartRTTs * rtt
	if w < base {
		w = base
	}
	if w > ceil {
		w = ceil
	}
	return w
}

// laneStaggerStep is the per-lane spawn delay, shrunk so even the last lane (of
// up to 128) spawns within half the warmup window — laneStagger is only the cap.
// 0 ⇒ one lane or no warmup ⇒ spawn together.
func (r *runner) laneStaggerStep() time.Duration {
	if r.streams <= 1 {
		return 0
	}
	step := adaptiveWarmup(r.cfg.Warmup, r.idleRTT) / 2 / time.Duration(r.streams-1)
	if step > laneStagger {
		step = laneStagger
	}
	return step
}

// staggerSleep delays lane `lane` by lane*step (lane 0 / step 0 are immediate),
// returning false if the context is cancelled during the wait.
func staggerSleep(ctx context.Context, lane int, step time.Duration) bool {
	delay := time.Duration(lane) * step
	if delay <= 0 {
		return true
	}
	t := time.NewTimer(delay)
	defer t.Stop()
	select {
	case <-ctx.Done():
		return false
	case <-t.C:
		return true
	}
}

func (r *runner) fail(err error) error {
	if err == nil || errors.Is(err, context.Canceled) {
		return err
	}
	r.emit(Event{Kind: EventError, At: time.Now(), Err: err})
	return err
}

func (r *runner) runLatencyStage(ctx context.Context, stage string, underLoad bool, duration time.Duration) error {
	start, err := r.warmupGate(ctx, stage)
	if err != nil {
		return err
	}
	stats, err := r.measureLatency(ctx, stage, underLoad, duration, start)
	if err != nil {
		return err
	}
	// Capture idle RTT so the later throughput stages' warmup can scale to it.
	if !underLoad && stats.P50 > 0 {
		r.idleRTT = stats.P50
	}
	res := Result{Stage: stage, Latency: stats, Samples: stats.Count, Elapsed: duration}
	r.emit(Event{Kind: EventResult, At: time.Now(), Stage: stage, Result: &res})
	return nil
}

func (r *runner) runTransferStage(ctx context.Context, stage string, dirs []Direction, duration time.Duration) error {
	start, err := r.warmupGate(ctx, stage)
	if err != nil {
		return err
	}

	stageCtx, cancelStage := context.WithCancel(ctx)
	defer cancelStage()

	var wg sync.WaitGroup
	errs := make(chan error, len(dirs)+1)
	results := make(chan Result, len(dirs))
	for _, dir := range dirs {
		dir := dir
		wg.Add(1)
		go func() {
			defer wg.Done()
			var res Result
			var err error
			if dir == Down {
				res, err = r.measureDownload(stageCtx, stage, duration, start)
			} else {
				res, err = r.measureUpload(stageCtx, stage, duration, start)
			}
			if err != nil && !errors.Is(err, context.Canceled) && !errors.Is(err, context.DeadlineExceeded) {
				errs <- err
				return
			}
			results <- res
		}()
	}

	latDone := make(chan struct{})
	if r.cfg.LoadedLatency {
		wg.Add(1)
		go func() {
			defer wg.Done()
			defer close(latDone)
			stats, err := r.measureLatency(stageCtx, stage, true, duration, start)
			if err != nil && !errors.Is(err, context.Canceled) && !errors.Is(err, context.DeadlineExceeded) {
				errs <- err
				return
			}
			if stats.Count > 0 {
				res := Result{Stage: stage, Latency: stats, Samples: stats.Count, Elapsed: duration}
				r.emit(Event{Kind: EventResult, At: time.Now(), Stage: stage, Result: &res})
			}
		}()
	} else {
		close(latDone)
	}

	done := make(chan struct{})
	go func() {
		wg.Wait()
		close(done)
	}()

	select {
	case <-done:
	case err := <-errs:
		cancelStage()
		<-done
		return err
	case <-ctx.Done():
		cancelStage()
		<-done
		return ctx.Err()
	}
	close(results)
	for res := range results {
		r.emit(Event{Kind: EventResult, At: time.Now(), Stage: stage, Direction: res.Direction, Result: &res})
	}
	<-latDone
	return nil
}

func (r *runner) warmupGate(ctx context.Context, stage string) (<-chan struct{}, error) {
	start := make(chan struct{})
	warmup := adaptiveWarmup(r.cfg.Warmup, r.idleRTT)
	if warmup <= 0 {
		r.emit(Event{Kind: EventStage, At: time.Now(), Stage: stage, Message: "measure"})
		close(start)
		return start, nil
	}
	r.emit(Event{Kind: EventStage, At: time.Now(), Stage: stage, Message: "warmup"})
	go func() {
		timer := time.NewTimer(warmup)
		defer timer.Stop()
		select {
		case <-ctx.Done():
		case <-timer.C:
			r.emit(Event{Kind: EventStage, At: time.Now(), Stage: stage, Message: "measure"})
			close(start)
		}
	}()
	return start, nil
}

func (r *runner) endpoint(path string) (string, error) {
	if path == "" {
		return "", fmt.Errorf("empty endpoint path")
	}
	base := r.cfg.BaseURL
	if r.target != nil {
		base = r.target.Origin
	}
	return httpEndpoint(base, path)
}

func (r *runner) routes() wire.Routes {
	if r.target != nil {
		return r.target.Routes
	}
	return wire.DefaultRoutes(true)
}

func selectTarget(cfg Config, pf wire.Preflight) (*wire.Target, string, error) {
	targets := map[string]*wire.Target{"http1": pf.Capabilities.Targets.HTTP1, "http2": pf.Capabilities.Targets.HTTP2, "http3": pf.Capabilities.Targets.HTTP3}
	protocol := cfg.Protocol
	if protocol == "invalid" {
		return nil, "", fmt.Errorf("protocol must be auto, http1, http2, or http3")
	}
	if protocol == "auto" {
		base, err := url.Parse(cfg.BaseURL)
		if err != nil {
			return nil, "", err
		}
		for _, name := range []string{"http1", "http2", "http3"} {
			if t := targets[name]; t != nil {
				u, _ := url.Parse(t.Origin)
				if u.Scheme == base.Scheme && u.Host == base.Host {
					return t, name, nil
				}
			}
		}
		protocol = "http1"
	}
	if targets[protocol] == nil {
		return nil, "", fmt.Errorf("%s target unavailable", protocol)
	}
	return targets[protocol], protocol, nil
}

func protocolClient(cfg Config, protocol string, makeHTTP func() *http.Transport) (*http.Client, func()) {
	tlsConfig := &tls.Config{InsecureSkipVerify: cfg.InsecureSkipTLSVerify} //nolint:gosec
	if protocol == "http3" {
		tr := &http3.Transport{TLSClientConfig: tlsConfig}
		return &http.Client{Transport: tr}, func() { _ = tr.Close() }
	}
	tr := makeHTTP()
	tr.TLSClientConfig = tlsConfig
	p := &http.Protocols{}
	p.SetHTTP1(protocol == "http1")
	p.SetHTTP2(protocol == "http2")
	tr.Protocols = p
	return &http.Client{Transport: tr}, tr.CloseIdleConnections
}
