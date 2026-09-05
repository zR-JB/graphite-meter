package goclient

import (
	"cmp"
	"context"
	"crypto/tls"
	"errors"
	"fmt"
	"net"
	"net/http"
	"net/url"
	"strings"
	"sync"
	"time"

	"github.com/quic-go/quic-go/http3"

	"github.com/zR-JB/graphite-meter/go/internal/origin"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

type PreparedConnection struct {
	Preflight        wire.Preflight
	ThroughputTarget wire.ThroughputTarget
	LatencyTarget    *wire.LatencyTarget
	Probe            wire.Probe
	LatencyProbe     *wire.Probe
	VerifiedAt       time.Time
	configKey        string
}

type PreparationError struct {
	Preflight wire.Preflight
	Err       error
}

func (e *PreparationError) Error() string { return e.Err.Error() }
func (e *PreparationError) Unwrap() error { return e.Err }

const preparationFreshness = 30 * time.Second

func preparationKey(cfg Config) string {
	return fmt.Sprintf("%s\n%s\n%s\n%s\n%s\n%s\n%s\n%t\n%t\n%t", cfg.BaseURL, cfg.ThroughputTarget, cfg.ThroughputProtocol, cfg.ThroughputTransport, cfg.LatencyTarget, cfg.LatencyTransport, cfg.PingInterval, cfg.InsecureSkipTLSVerify, cfg.needsLatency(), cfg.authToken() != "")
}

func (c Config) needsLatency() bool {
	return c.Stages.Latency || (c.LoadedLatency && (c.Stages.Download || c.Stages.Upload || c.Stages.Bidirectional))
}

func canonicalOrigin(raw string) (string, error) {
	u, err := url.Parse(raw)
	if err != nil || (u.Scheme != "http" && u.Scheme != "https") || u.Host == "" || u.User != nil || (u.Path != "" && u.Path != "/") || u.RawQuery != "" || u.ForceQuery || u.Fragment != "" {
		return "", errors.New("invalid server URL")
	}
	return strings.ToLower(u.Scheme) + "://" + strings.ToLower(u.Host), nil
}

func CanonicalServerOrigin(raw string) (string, error) { return canonicalOrigin(raw) }

func (c Config) authToken() string {
	serverOrigin, err := canonicalOrigin(c.BaseURL)
	if err != nil || !strings.EqualFold(serverOrigin, c.AuthOrigin) {
		return ""
	}
	return c.AuthToken
}

type authTransport struct {
	token, hostname string
	base            http.RoundTripper
}

func (t authTransport) RoundTrip(r *http.Request) (*http.Response, error) {
	if t.token == "" {
		return t.base.RoundTrip(r)
	}
	if r.URL.Scheme != "https" || !strings.EqualFold(r.URL.Hostname(), t.hostname) {
		return nil, fmt.Errorf("refusing to send authentication grant outside canonical HTTPS host")
	}
	clone := r.Clone(r.Context())
	clone.Header = r.Header.Clone()
	clone.Header.Set("Authorization", "Bearer "+t.token)
	return t.base.RoundTrip(clone)
}

func pinnedHostname(origin string) string {
	u, err := url.Parse(origin)
	if err != nil {
		return ""
	}
	return u.Hostname()
}

func authenticatedClient(cfg Config, base http.RoundTripper) *http.Client {
	token := cfg.authToken()
	client := &http.Client{Transport: authTransport{token: token, hostname: pinnedHostname(cfg.AuthOrigin), base: base}}
	if token != "" {
		client.CheckRedirect = func(*http.Request, []*http.Request) error {
			return errors.New("authenticated measurement endpoints must not redirect")
		}
	}
	return client
}

func (p *PreparedConnection) FreshFor(cfg Config) bool {
	return p != nil && p.configKey == preparationKey(cfg.normalized()) && time.Since(p.VerifiedAt) <= preparationFreshness
}

func ConnectionSummary(transport, protocol string, tls bool) string {
	mechanism := map[string]string{
		wire.TransportWebSocket:            "WebSocket",
		wire.TransportWebTransport:         "WebTransport",
		wire.TransportWebTransportDatagram: "WebTransport datagrams",
	}[transport]
	mechanism = cmp.Or(mechanism, "Fetch stream")
	security := "clear"
	if tls {
		security = "TLS"
	}
	return fmt.Sprintf("%s · %s · %s", mechanism, ProtocolLabel(protocol), security)
}

func ProtocolLabel(protocol string) string {
	switch protocolFromEvidence(protocol) {
	case "http1":
		return "HTTP/1.1"
	case "http2":
		return "HTTP/2"
	case "http3":
		return "HTTP/3"
	case "negotiated":
		return "Negotiated"
	case "":
		return "--"
	}
	return protocol
}

func (p *PreparedConnection) ThroughputSummary() string {
	if p == nil {
		return "Not checked"
	}
	t := p.ThroughputTarget
	return ConnectionSummary(t.Transport, t.Protocol, t.TLS)
}

func (p *PreparedConnection) LatencySummary() string {
	if p == nil || p.LatencyTarget == nil {
		return "Not selected"
	}
	t := p.LatencyTarget
	return ConnectionSummary(t.Transport, t.Protocol, t.TLS)
}

func baseTransport(cfg Config) *http.Transport {
	return &http.Transport{
		Proxy:                 http.ProxyFromEnvironment,
		DialContext:           (&net.Dialer{Timeout: 10 * time.Second, KeepAlive: 30 * time.Second}).DialContext,
		ForceAttemptHTTP2:     true,
		MaxIdleConns:          cfg.MaxIdleConnsPerHost * 2,
		MaxIdleConnsPerHost:   cfg.MaxIdleConnsPerHost,
		MaxConnsPerHost:       0,
		IdleConnTimeout:       90 * time.Second,
		ResponseHeaderTimeout: cfg.ResponseHeaderTimeout,
		ExpectContinueTimeout: cfg.ExpectContinueTimeout,
		TLSClientConfig:       &tls.Config{InsecureSkipVerify: cfg.InsecureSkipTLSVerify}, //nolint:gosec
		WriteBufferSize:       256 * 1024,
		ReadBufferSize:        256 * 1024,
	}
}

func websocketClient(cfg Config) (*http.Client, func()) {
	tr := baseTransport(cfg)
	protocols := &http.Protocols{}
	protocols.SetHTTP1(true)
	tr.Protocols = protocols
	return authenticatedClient(cfg, tr), tr.CloseIdleConnections
}

func Prepare(ctx context.Context, cfg Config) (*PreparedConnection, error) {
	cfg = cfg.normalized()
	if cfg.authToken() != "" {
		u, err := url.Parse(cfg.BaseURL)
		if err != nil || u.Scheme != "https" || cfg.InsecureSkipTLSVerify {
			return nil, fmt.Errorf("authenticated operation requires verified HTTPS -url")
		}
	}
	switch cfg.ThroughputProtocol {
	case "auto", "http1", "http2", "http3":
	default:
		return nil, fmt.Errorf("invalid throughput protocol %q", cfg.ThroughputProtocol)
	}
	if err := ValidateThroughputTransport(cfg.ThroughputTransport); err != nil {
		return nil, err
	}
	if err := ValidateLatencyTransport(cfg.LatencyTransport); err != nil {
		return nil, err
	}
	discoveryTransport := baseTransport(cfg)
	defer discoveryTransport.CloseIdleConnections()
	discoveryClient := authenticatedClient(cfg, discoveryTransport)

	pf, err := getPreflight(ctx, discoveryClient, cfg.BaseURL)
	if err != nil {
		return nil, err
	}
	fail := func(err error) (*PreparedConnection, error) {
		return nil, &PreparationError{Preflight: pf, Err: err}
	}
	advertisedTarget, err := selectTarget(cfg, pf)
	if err != nil {
		return fail(err)
	}
	if advertisedTarget.Transport == wire.TransportWebTransport {
		if verifyErr := verifyThroughputWebTransport(ctx, cfg, advertisedTarget); verifyErr != nil {
			if cfg.ThroughputTransport != "auto" {
				return fail(verifyErr)
			}
			fetchTarget, fetchErr := selectTargetOver(cfg, pf, wire.TransportFetchStream)
			if fetchErr != nil {
				return fail(fmt.Errorf("%w (the advertised WebTransport target is unreachable: %v)", fetchErr, verifyErr))
			}
			advertisedTarget = fetchTarget
		}
	}
	target := *advertisedTarget
	if cfg.ThroughputProtocol != "auto" {
		if target.Protocol != "negotiated" && target.Protocol != cfg.ThroughputProtocol {
			return fail(fmt.Errorf("endpoint is fixed to %s, cannot use %s", target.Protocol, cfg.ThroughputProtocol))
		}
		target.Protocol = cfg.ThroughputProtocol
	}
	transfer, closeTransfer := protocolClient(cfg, target.Protocol, func() *http.Transport { return baseTransport(cfg) })
	defer closeTransfer()
	probe, clientProtocol, err := getJSONProbe(ctx, transfer, target.Origin, target.Routes.Probe, "probe")
	if err != nil {
		return fail(err)
	}
	if target.Protocol == "negotiated" {
		target.Protocol = protocolFromEvidence(clientProtocol)
	}
	wsClient, closeWebSocket := websocketClient(cfg)
	defer closeWebSocket()
	latencyTarget, latencyErr := selectLatencyTarget(cfg, pf.Capabilities.LatencyTargets)
	needsLatency := cfg.needsLatency()
	if latencyErr != nil && needsLatency {
		return fail(latencyErr)
	}
	var latencyProbe *wire.Probe
	if !needsLatency {
		latencyTarget = nil
	} else if latencyTarget != nil {
		if cfg.LatencyTransport != "auto" && PingIntervalBoundApplies(latencyTarget.Transport) {
			if err := ValidatePingInterval(cfg.PingInterval); err != nil {
				return fail(err)
			}
		}
		if latencyTarget.Transport == wire.TransportWebTransport {
			if verifyErr := verifyLatencyWebTransport(ctx, cfg, latencyTarget); verifyErr != nil {
				if cfg.LatencyTransport != "auto" {
					return fail(verifyErr)
				}
				if latencyTarget, latencyErr = selectLatencyTargetOver(cfg.LatencyTarget, cfg.BaseURL, pf.Capabilities.LatencyTargets, wire.TransportWebSocket); latencyErr != nil {
					return fail(latencyErr)
				}
			}
		}
		if cfg.LatencyTransport == "auto" && PingIntervalBoundApplies(latencyTarget.Transport) {
			if err := ValidatePingInterval(cfg.PingInterval); err != nil {
				return fail(err)
			}
		}
		p, _, err := getJSONProbe(ctx, wsClient, latencyTarget.Origin, latencyTarget.Routes.Probe, "latency probe")
		if err != nil {
			return fail(err)
		}
		latencyProbe = new(p)
		if latencyTarget.Transport == wire.TransportWebSocket {
			if err := verifyLatencyWebSocket(ctx, wsClient, latencyTarget); err != nil {
				return fail(err)
			}
		}
	}
	return &PreparedConnection{Preflight: pf, ThroughputTarget: target, LatencyTarget: latencyTarget, Probe: probe, LatencyProbe: latencyProbe, VerifiedAt: time.Now(), configKey: preparationKey(cfg)}, nil
}

func Run(ctx context.Context, cfg Config, emit func(Event)) error {
	prepared, err := Prepare(ctx, cfg)
	if err != nil {
		emit(Event{Kind: EventError, At: time.Now(), Err: err})
		return err
	}
	return RunPrepared(ctx, cfg, prepared, emit)
}

func RunPrepared(ctx context.Context, cfg Config, prepared *PreparedConnection, emit func(Event)) error {
	cfg = cfg.normalized()
	if prepared == nil || prepared.configKey != preparationKey(cfg) || time.Since(prepared.VerifiedAt) > preparationFreshness {
		return Run(ctx, cfg, emit)
	}
	target := &prepared.ThroughputTarget
	transfer, closeTransfer := protocolClient(cfg, target.Protocol, func() *http.Transport { return baseTransport(cfg) })
	defer closeTransfer()
	wsClient, closeWebSocket := websocketClient(cfg)
	defer closeWebSocket()
	pf := prepared.Preflight
	probe := prepared.Probe
	latencyTarget := prepared.LatencyTarget
	latencyProbe := prepared.LatencyProbe
	throughputProtocol := targetProtocolEvidence(target.Protocol)
	if target.Protocol == "negotiated" {
		throughputProtocol = probe.ProtocolNegotiated
	}
	event := Event{Kind: EventPreflight, At: time.Now(), Preflight: new(pf), Probe: new(probe), LatencyProbe: latencyProbe, Message: target.ID, ThroughputTarget: target.ID, ThroughputProtocol: throughputProtocol, ThroughputTransport: target.Transport}
	if latencyTarget != nil {
		event.LatencyTarget = latencyTarget.ID
		event.LatencyTransport = latencyTarget.Transport
		event.LatencyProtocol = latencyBusEvidence(latencyTarget, latencyProbe)
	}
	emit(event)

	r := runner{
		cfg: cfg, streams: cfg.TransferStreams.lanes(target.Protocol, target.Transport),
		http: transfer, websocketHTTP: wsClient,
		target: target, latencyTarget: latencyTarget,
		emit: emit,
	}
	for _, stage := range []struct {
		name     string
		enabled  bool
		duration time.Duration
		dirs     []Direction
	}{
		{"latency", cfg.Stages.Latency, cfg.LatencyDuration, nil},
		{"download", cfg.Stages.Download, cfg.DownloadDuration, []Direction{Down}},
		{"upload", cfg.Stages.Upload, cfg.UploadDuration, []Direction{Up}},
		{"bidirectional", cfg.Stages.Bidirectional, cfg.BidirectionalDuration, []Direction{Down, Up}},
	} {
		if !stage.enabled {
			continue
		}
		var err error
		if stage.dirs == nil {
			err = r.runLatencyStage(ctx, stage.name, false, stage.duration)
		} else {
			err = r.runTransferStage(ctx, stage.name, stage.dirs, stage.duration)
		}
		if err != nil {
			return r.fail(err)
		}
	}
	emit(Event{Kind: EventComplete, At: time.Now(), Message: "complete"})
	return nil
}

type runner struct {
	cfg           Config
	streams       streamCounts
	http          *http.Client
	websocketHTTP *http.Client
	target        *wire.ThroughputTarget
	latencyTarget *wire.LatencyTarget
	emit          func(Event)
	idleRTT       time.Duration
}

type transferOutcome struct {
	result  Result
	err     error
	latency bool
}

const laneStagger = 75 * time.Millisecond

func adaptiveWarmup(base, rtt time.Duration) time.Duration {
	const slowStartRTTs = 10
	const ceil = 4 * time.Second
	w := min(max(slowStartRTTs*rtt, base), ceil)
	return w
}

func (r *runner) laneStaggerStep(streams int) time.Duration {
	if streams <= 1 {
		return 0
	}
	step := min(adaptiveWarmup(r.cfg.Warmup, r.idleRTT)/2/time.Duration(streams-1), laneStagger)
	return step
}

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
	start := r.warmupGate(ctx, stage)
	stats, err := r.measureLatency(ctx, stage, underLoad, duration, start)
	if err != nil {
		return err
	}
	if !underLoad && stats.P50 > 0 {
		r.idleRTT = stats.P50
	}
	res := Result{Stage: stage, Latency: stats, Samples: stats.Count, Elapsed: duration}
	r.emit(Event{Kind: EventResult, At: time.Now(), Stage: stage, Result: new(res)})
	return nil
}

func stageFailed(err error) bool {
	return err != nil && !errors.Is(err, context.Canceled)
}

func (r *runner) runTransferStage(ctx context.Context, stage string, dirs []Direction, duration time.Duration) error {
	start := r.warmupGate(ctx, stage)

	stageCtx, cancelStage := context.WithCancel(ctx)
	defer cancelStage()

	var wg sync.WaitGroup
	outcomes := make(chan transferOutcome, len(dirs)+1)
	for _, dir := range dirs {
		wg.Go(func() {
			res, err := r.measureDirection(stageCtx, stage, dir, duration, start)
			outcomes <- transferOutcome{result: res, err: err}
		})
	}

	if r.cfg.LoadedLatency {
		wg.Go(func() {
			stats, err := r.measureLatency(stageCtx, stage, true, duration, start)
			if err == nil {
				outcomes <- transferOutcome{result: Result{Stage: stage, Latency: stats, Samples: stats.Count, Elapsed: duration}, latency: true}
				return
			}
			outcomes <- transferOutcome{result: Result{Stage: stage}, err: err, latency: true}
		})
	}

	remaining := len(dirs)
	if r.cfg.LoadedLatency {
		remaining++
	}
	collected := make([]transferOutcome, 0, remaining)
	for remaining > 0 {
		select {
		case outcome := <-outcomes:
			if stageFailed(outcome.err) {
				cancelStage()
				wg.Wait()
				return outcome.err
			}
			collected = append(collected, outcome)
			remaining--
		case <-ctx.Done():
			cancelStage()
			wg.Wait()
			return ctx.Err()
		}
	}
	wg.Wait()
	if err := ctx.Err(); err != nil {
		return err
	}
	for _, outcome := range collected {
		if !outcome.latency {
			continue
		}
		if outcome.err == nil {
			r.emit(Event{Kind: EventResult, At: time.Now(), Stage: stage, Result: new(outcome.result)})
		}
	}
	for _, outcome := range collected {
		if outcome.latency {
			continue
		}
		r.emit(Event{Kind: EventResult, At: time.Now(), Stage: stage, Direction: outcome.result.Direction, Result: new(outcome.result)})
	}
	return nil
}

func (r *runner) measureDirection(ctx context.Context, stage string, dir Direction, duration time.Duration, start <-chan struct{}) (Result, error) {
	if dir == Down {
		return r.measureDownload(ctx, stage, duration, start)
	}
	return r.measureUpload(ctx, stage, duration, start)
}

func (r *runner) warmupGate(ctx context.Context, stage string) <-chan struct{} {
	start := make(chan struct{})
	warmup := adaptiveWarmup(r.cfg.Warmup, r.idleRTT)
	if warmup <= 0 {
		r.emit(Event{Kind: EventStage, At: time.Now(), Stage: stage, Message: "measure"})
		close(start)
		return start
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
	return start
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

func (r *runner) targetTransport() string {
	if r.target == nil {
		return wire.TransportFetchStream
	}
	return r.target.Transport
}

func (r *runner) routes() wire.ThroughputRoutes {
	if r.target != nil {
		return r.target.Routes
	}
	return wire.DefaultThroughputRoutes()
}

func transportOrder(selection string, preferred, fallback string) []string {
	if selection != "auto" {
		return []string{selection}
	}
	return []string{preferred, fallback}
}

func selectTarget(cfg Config, pf wire.Preflight) (*wire.ThroughputTarget, error) {
	if cfg.ThroughputTransport == wire.TransportWebTransportDatagram {
		return nil, fmt.Errorf("webtransport-datagram throughput is not supported by this client")
	}
	for _, mechanism := range transportOrder(cfg.ThroughputTransport, wire.TransportFetchStream, wire.TransportWebTransport) {
		t, err := selectTargetOver(cfg, pf, mechanism)
		if err == nil {
			return t, nil
		}
		if cfg.ThroughputTransport != "auto" {
			return nil, err
		}
	}
	return nil, fmt.Errorf("%s target unavailable", cfg.ThroughputTarget)
}

func selectTargetOver(cfg Config, pf wire.Preflight, mechanism string) (*wire.ThroughputTarget, error) {
	selection := cfg.ThroughputTarget
	if selection == "auto" {
		for i := range pf.Capabilities.ThroughputTargets {
			t := &pf.Capabilities.ThroughputTargets[i]
			if t.Transport != mechanism {
				continue
			}
			if origin.Equal(t.Origin, cfg.BaseURL) {
				return t, nil
			}
		}
		var candidate *wire.ThroughputTarget
		for i := range pf.Capabilities.ThroughputTargets {
			t := &pf.Capabilities.ThroughputTargets[i]
			if t.Transport == mechanism {
				if candidate != nil {
					return nil, fmt.Errorf("multiple throughput endpoints available; select an origin")
				}
				candidate = t
			}
		}
		if candidate != nil {
			return candidate, nil
		}
	}
	for i := range pf.Capabilities.ThroughputTargets {
		t := &pf.Capabilities.ThroughputTargets[i]
		if t.Transport == mechanism && (t.ID == selection || origin.Equal(t.Origin, selection)) {
			return t, nil
		}
	}
	return nil, fmt.Errorf("%s target unavailable over %s", selection, mechanism)
}

func latencyBusEvidence(target *wire.LatencyTarget, probe *wire.Probe) string {
	if target.Transport == wire.TransportWebTransport {
		return targetProtocolEvidence("http3")
	}
	if probe == nil {
		return ""
	}
	return probe.ProtocolNegotiated
}

func targetProtocolEvidence(protocol string) string {
	switch protocol {
	case "http1":
		return "http/1.1"
	case "http2":
		return "h2"
	case "http3":
		return "h3"
	default:
		return protocol
	}
}

func protocolFromEvidence(protocol string) string {
	switch protocol {
	case "http/1.1", "HTTP/1.1":
		return "http1"
	case "h2", "HTTP/2.0":
		return "http2"
	case "h3", "HTTP/3.0":
		return "http3"
	}
	return protocol
}

func selectLatencyTarget(cfg Config, targets []wire.LatencyTarget) (*wire.LatencyTarget, error) {
	for _, mechanism := range transportOrder(cfg.LatencyTransport, wire.TransportWebTransport, wire.TransportWebSocket) {
		t, err := selectLatencyTargetOver(cfg.LatencyTarget, cfg.BaseURL, targets, mechanism)
		if err == nil {
			return t, nil
		}
		if cfg.LatencyTransport != "auto" {
			return nil, err
		}
	}
	return nil, fmt.Errorf("latency target %q unavailable", cfg.LatencyTarget)
}

func selectLatencyTargetOver(selection, base string, targets []wire.LatencyTarget, mechanism string) (*wire.LatencyTarget, error) {
	var candidate *wire.LatencyTarget
	var sameOriginCandidate *wire.LatencyTarget
	candidateCount := 0
	for i := range targets {
		t := &targets[i]
		if t.Transport != mechanism {
			continue
		}
		if selection != "auto" && (t.ID == selection || origin.Equal(t.Origin, selection)) {
			return t, nil
		}
		if selection == "auto" {
			candidateCount++
			if candidate == nil {
				candidate = t
			}
			if sameOriginCandidate == nil && origin.Equal(t.Origin, base) {
				sameOriginCandidate = t
			}
		}
	}
	if selection == "auto" {
		if sameOriginCandidate != nil {
			return sameOriginCandidate, nil
		}
		if candidateCount == 1 {
			return candidate, nil
		}
		if candidateCount > 1 {
			selection = "ambiguous"
		}
	}
	return nil, fmt.Errorf("latency target %q unavailable", selection)
}

func protocolClient(cfg Config, protocol string, makeHTTP func() *http.Transport) (*http.Client, func()) {
	tlsConfig := &tls.Config{InsecureSkipVerify: cfg.InsecureSkipTLSVerify} //nolint:gosec
	if protocol == "http3" {
		tr := &http3.Transport{TLSClientConfig: tlsConfig, QUICConfig: transport.NewQUICConfig()}
		return authenticatedClient(cfg, tr), func() { _ = tr.Close() }
	}
	tr := makeHTTP()
	tr.TLSClientConfig = tlsConfig
	if protocol != "negotiated" {
		p := &http.Protocols{}
		p.SetHTTP1(protocol == "http1")
		p.SetHTTP2(protocol == "http2")
		tr.Protocols = p
	}
	return authenticatedClient(cfg, tr), tr.CloseIdleConnections
}
