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
	PreflightRTT     time.Duration
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
	return fmt.Sprintf("%s\n%s\n%s\n%s\n%s\n%s\n%s\n%t\n%t\n%t", cfg.BaseURL, cfg.ThroughputTarget, cfg.ThroughputProtocol, cfg.ThroughputTransport, cfg.LatencyTarget, cfg.LatencyTransport, cfg.PingInterval, cfg.InsecureSkipTLSVerify, cfg.needsLatency(), cfg.grant != "")
}

// authTransport sends a grant only to its issuer's HTTPS hostname.
type authTransport struct {
	token, hostname string
	base            http.RoundTripper
}

func (t authTransport) RoundTrip(r *http.Request) (*http.Response, error) {
	if t.token == "" {
		return t.base.RoundTrip(r)
	}
	// Additional catalogue origins never receive the grant.
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
	client := &http.Client{Transport: authTransport{token: cfg.grant, hostname: pinnedHostname(cfg.BaseURL), base: base}}
	if cfg.grant != "" || cfg.server != nil {
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
		MaxIdleConns:          maxIdleConnsPerHost * 2,
		MaxIdleConnsPerHost:   maxIdleConnsPerHost,
		IdleConnTimeout:       90 * time.Second,
		ResponseHeaderTimeout: responseHeaderTimeout,
		ExpectContinueTimeout: expectContinueTimeout,
		TLSClientConfig:       &tls.Config{InsecureSkipVerify: cfg.InsecureSkipTLSVerify}, //nolint:gosec
		WriteBufferSize:       256 * 1024,
		ReadBufferSize:        256 * 1024,
		// Measured bytes are wire payload.
		DisableCompression: true,
		// The 4 MiB default stream window caps H2 downloads per RTT; 64 MiB bounds unread data.
		HTTP2: &http.HTTP2Config{MaxReceiveBufferPerStream: 32 << 20, MaxReceiveBufferPerConnection: 64 << 20},
	}
}

func websocketClient(cfg Config) (*http.Client, func()) {
	tr := baseTransport(cfg)
	protocols := &http.Protocols{}
	protocols.SetHTTP1(true)
	tr.Protocols = protocols
	return authenticatedClient(cfg, tr), tr.CloseIdleConnections
}

// prepare checks one server's discovery and both of its paths.
func prepare(ctx context.Context, cfg Config) (*PreparedConnection, error) {
	cfg = cfg.normalized()
	if cfg.grant != "" {
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
	if cfg.server != nil {
		if err := cfg.server.ValidateDiscovery(pf); err != nil {
			return nil, &PreparationError{Preflight: pf, Err: err}
		}
	}
	// Both paths are checked concurrently, so a blocked UDP path costs one timeout.
	branches, cancel := context.WithCancel(ctx)
	defer cancel()
	prepared := &PreparedConnection{Preflight: pf, configKey: preparationKey(cfg)}
	var throughputErr, latencyErr error
	var work sync.WaitGroup
	work.Go(func() {
		if throughputErr = prepareThroughput(branches, cfg, prepared); throughputErr != nil {
			cancel()
		}
	})
	if cfg.needsLatency() {
		work.Go(func() {
			if latencyErr = prepareLatency(branches, cfg, prepared); latencyErr != nil {
				cancel()
			}
		})
	}
	work.Wait()
	// Report the failure, not the sibling it cancelled.
	if err := throughputErr; err != nil || latencyErr != nil {
		if err == nil || errors.Is(err, context.Canceled) && latencyErr != nil && ctx.Err() == nil {
			err = latencyErr
		}
		return nil, &PreparationError{Preflight: pf, Err: err}
	}
	prepared.VerifiedAt = time.Now()
	return prepared, nil
}

func prepareThroughput(ctx context.Context, cfg Config, prepared *PreparedConnection) error {
	pf := prepared.Preflight
	advertisedTarget, err := selectTarget(cfg, pf)
	if err != nil {
		return err
	}
	if advertisedTarget.Transport == wire.TransportWebTransport {
		if verifyErr := verifyThroughputWebTransport(ctx, cfg, advertisedTarget); verifyErr != nil {
			if cfg.ThroughputTransport != "auto" {
				return verifyErr
			}
			fetchTarget, fetchErr := selectTargetOver(cfg, pf, wire.TransportFetchStream)
			if fetchErr != nil {
				return fmt.Errorf("%w (the advertised WebTransport target is unreachable: %v)", fetchErr, verifyErr)
			}
			advertisedTarget = fetchTarget
		}
	}
	target := *advertisedTarget
	if cfg.ThroughputProtocol != "auto" {
		if target.Protocol != "negotiated" && target.Protocol != cfg.ThroughputProtocol {
			return fmt.Errorf("endpoint is fixed to %s, cannot use %s", target.Protocol, cfg.ThroughputProtocol)
		}
		target.Protocol = cfg.ThroughputProtocol
	}
	transfer, closeTransfer := protocolClient(cfg, target.Protocol, func() *http.Transport { return baseTransport(cfg) })
	defer closeTransfer()
	probe, clientProtocol, err := getJSONProbe(ctx, transfer, target.Origin, target.Routes.Probe, "probe")
	if err != nil {
		return err
	}
	if target.Protocol == "negotiated" {
		target.Protocol = protocolFromEvidence(clientProtocol)
	}
	prepared.ThroughputTarget, prepared.Probe = target, probe
	return nil
}

func prepareLatency(ctx context.Context, cfg Config, prepared *PreparedConnection) error {
	targets := prepared.Preflight.Capabilities.LatencyTargets
	target, err := selectLatencyTarget(cfg, targets)
	if err != nil {
		return err
	}
	if cfg.LatencyTransport != "auto" && PingIntervalBoundApplies(target.Transport) {
		if err := ValidatePingInterval(cfg.PingInterval); err != nil {
			return err
		}
	}
	if target.Transport == wire.TransportWebTransport {
		if verifyErr := verifyLatencyWebTransport(ctx, cfg, target); verifyErr != nil {
			if cfg.LatencyTransport != "auto" {
				return verifyErr
			}
			if target, err = selectLatencyTargetOver(cfg.LatencyTarget, cfg.BaseURL, targets, wire.TransportWebSocket); err != nil {
				return err
			}
		}
	}
	if cfg.LatencyTransport == "auto" && PingIntervalBoundApplies(target.Transport) {
		if err := ValidatePingInterval(cfg.PingInterval); err != nil {
			return err
		}
	}
	wsClient, closeWebSocket := websocketClient(cfg)
	defer closeWebSocket()
	probeStarted := time.Now()
	probe, _, err := getJSONProbe(ctx, wsClient, target.Origin, target.Routes.Probe, "latency probe")
	if err != nil {
		return err
	}
	rtt := time.Since(probeStarted)
	if target.Transport == wire.TransportWebSocket {
		if err := verifyLatencyWebSocket(ctx, wsClient, target); err != nil {
			return err
		}
	}
	prepared.LatencyTarget, prepared.LatencyProbe, prepared.PreflightRTT = target, &probe, rtt
	return nil
}

// runner owns one server's connections within a run.
type runner struct {
	coordinated   *participantCounters
	cfg           Config
	streams       streamCounts
	http          *http.Client
	websocketHTTP *http.Client
	target        *wire.ThroughputTarget
	latencyTarget *wire.LatencyTarget
	emit          func(Event)
	idleRTT       time.Duration
	teardown      context.Context // Releases server state after measurement stops; nil derives it from the stage.
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

func (r *runner) measureDirection(ctx context.Context, dir Direction, gate *stageGate) error {
	if dir == Down {
		return r.measureDownload(ctx, gate)
	}
	return r.measureUpload(ctx, gate)
}

// Stage transport setup is bounded separately from warmup and the measured window.
const stageReadyTimeout = 10 * time.Second

type stageGate struct {
	reportReady   func()
	boundaryStart time.Time
	cancel        context.CancelCauseFunc
	start         chan struct{}
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
			if t.Transport != mechanism || cfg.ThroughputProtocol != "" && cfg.ThroughputProtocol != "auto" && t.Protocol != "negotiated" && t.Protocol != cfg.ThroughputProtocol {
				continue
			}
			if origin.Equal(t.Origin, cfg.BaseURL) {
				return t, nil
			}
		}
		var candidate *wire.ThroughputTarget
		for i := range pf.Capabilities.ThroughputTargets {
			t := &pf.Capabilities.ThroughputTargets[i]
			if t.Transport == mechanism && (cfg.ThroughputProtocol == "" || cfg.ThroughputProtocol == "auto" || t.Protocol == "negotiated" || t.Protocol == cfg.ThroughputProtocol) {
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
		tr := &http3.Transport{TLSClientConfig: tlsConfig, QUICConfig: transport.NewQUICConfig(), DisableCompression: true}
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
