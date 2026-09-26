package goclient

import (
	"cmp"
	"context"
	"crypto/tls"
	"errors"
	"fmt"
	"github.com/zR-JB/graphite-meter/go/internal/route"
	"net"
	"net/http"
	"net/url"
	"slices"
	"strings"
	"sync"
	"time"

	"github.com/quic-go/quic-go/http3"

	"github.com/zR-JB/graphite-meter/go/internal/origin"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

type PreparedConnection struct {
	WarmRTT          time.Duration
	Preflight        wire.Preflight
	ThroughputTarget wire.ThroughputTarget
	LatencyTarget    *wire.LatencyTarget
	VerifiedAt       time.Time
	configKey        string
	grantOrigins     []string
}

type PreparationError struct {
	Preflight wire.Preflight
	Err       error
}

func (e *PreparationError) Error() string { return e.Err.Error() }
func (e *PreparationError) Unwrap() error { return e.Err }

const preparationFreshness = 30 * time.Second

func preparationKey(cfg Config) string {
	return fmt.Sprintf("%s\n%s\n%s\n%s\n%s\n%s\n%s\n%t\n%t\n%t", cfg.BaseURL, cfg.ThroughputTarget,
		cfg.ThroughputProtocol, cfg.ThroughputTransport, cfg.LatencyTarget, cfg.LatencyTransport, cfg.PingInterval,
		cfg.InsecureSkipTLSVerify, cfg.needsLatency(), cfg.grant != "")
}

type authTransport struct {
	cfg  Config
	base http.RoundTripper
}

func (t authTransport) RoundTrip(r *http.Request) (*http.Response, error) {
	if t.cfg.grant == "" {
		return t.base.RoundTrip(r)
	}
	if !grantAllowed(r.URL, t.cfg) {
		return nil, fmt.Errorf("refusing to send authentication grant outside the server's HTTPS origins")
	}
	if t.cfg.InsecureSkipTLSVerify {
		return nil, fmt.Errorf("refusing to send authentication grant without TLS verification")
	}
	clone := r.Clone(r.Context())
	clone.Header = r.Header.Clone()
	clone.Header.Set("Authorization", "Bearer "+t.cfg.grant)
	return t.base.RoundTrip(clone)
}

func grantOrigins(base string, pf wire.Preflight) []string {
	origins := []string{base}
	sameHost := func(o string) {
		u, err := url.Parse(o)
		b, baseErr := url.Parse(base)
		if err == nil && baseErr == nil && strings.EqualFold(u.Hostname(), b.Hostname()) {
			origins = append(origins, o)
		}
	}
	for _, t := range pf.Capabilities.ThroughputTargets {
		sameHost(t.Origin)
	}
	for _, t := range pf.Capabilities.LatencyTargets {
		sameHost(t.Origin)
	}
	return origins
}

func grantAllowed(u *url.URL, cfg Config) bool {
	origins := cfg.grantOrigins
	if origins == nil {
		origins = []string{cfg.BaseURL}
	}
	here := u.Scheme + "://" + u.Host
	return u.Scheme == "https" && slices.ContainsFunc(origins, func(o string) bool { return origin.Equal(o, here) })
}

func authenticatedClient(cfg Config, base http.RoundTripper) *http.Client {
	client := &http.Client{Transport: authTransport{cfg, base}}
	if cfg.grant != "" || cfg.server != nil {
		client.CheckRedirect = func(*http.Request, []*http.Request) error {
			return errors.New("authenticated measurement endpoints must not redirect")
		}
	}
	return client
}

func (p *PreparedConnection) FreshFor(cfg Config) bool {
	return p != nil &&
		p.configKey == preparationKey(cfg.normalized()) &&
		time.Since(p.VerifiedAt) <= preparationFreshness
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
	return ConnectionSummary(t.Transport, t.Protocol, t.TLS())
}

func (p *PreparedConnection) LatencySummary() string {
	if p == nil || p.LatencyTarget == nil {
		return "Not selected"
	}
	t := p.LatencyTarget
	return ConnectionSummary(t.Transport, t.Protocol, t.TLS())
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
		DisableCompression:    true,
		// The 4 MiB default stream window caps H2 downloads per RTT.
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

func prepare(ctx context.Context, cfg Config) (*PreparedConnection, error) {
	cfg = cfg.normalized()
	if err := cfg.Validate(); err != nil {
		return nil, err
	}
	if cfg.grant != "" {
		u, err := url.Parse(cfg.BaseURL)
		if err != nil || u.Scheme != "https" || cfg.InsecureSkipTLSVerify {
			return nil, fmt.Errorf("authenticated operation requires verified HTTPS -url")
		}
	}
	discoveryTransport := baseTransport(cfg)
	defer discoveryTransport.CloseIdleConnections()
	pf, err := getPreflight(ctx, authenticatedClient(cfg, discoveryTransport), cfg.BaseURL)
	if err != nil {
		return nil, err
	}
	if cfg.server != nil {
		if err := cfg.server.ValidateDiscovery(pf); err != nil {
			return nil, &PreparationError{Preflight: pf, Err: err}
		}
	}
	cfg.grantOrigins = grantOrigins(cfg.BaseURL, pf)
	branches, cancel := context.WithCancel(ctx)
	defer cancel()
	prepared := &PreparedConnection{Preflight: pf, configKey: preparationKey(cfg), grantOrigins: cfg.grantOrigins}
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
	selected, err := selectTarget(cfg, pf)
	if err != nil {
		return err
	}
	if selected.Transport == wire.TransportWebTransport {
		if err := verifyThroughputWebTransport(ctx, cfg, selected); err != nil {
			if cfg.ThroughputTransport != "auto" {
				return err
			}
			_, fetchErr := throughputTargetOver(cfg, pf, wire.TransportFetchStream)
			return fmt.Errorf("%w (the advertised WebTransport target is unreachable: %v)", fetchErr, err)
		}
	}
	target := *selected
	if cfg.ThroughputProtocol != "auto" {
		if target.Protocol != "negotiated" && target.Protocol != cfg.ThroughputProtocol {
			return fmt.Errorf("endpoint is fixed to %s, cannot use %s", target.Protocol, cfg.ThroughputProtocol)
		}
		target.Protocol = cfg.ThroughputProtocol
	}
	transfer, closeTransfer := protocolClient(cfg, target.Protocol)
	defer closeTransfer()
	clientProtocol, err := getJSONProbe(ctx, transfer, target.Origin, target.Routes.Probe)
	if err != nil {
		return err
	}
	if target.Protocol == "negotiated" {
		target.Protocol = protocolFromEvidence(clientProtocol)
	}
	prepared.ThroughputTarget = target
	return nil
}

func prepareLatency(ctx context.Context, cfg Config, prepared *PreparedConnection) error {
	targets := prepared.Preflight.Capabilities.LatencyTargets
	target, err := selectLatencyTarget(cfg, targets)
	if err != nil {
		return err
	}
	wsClient, closeWebSocket := websocketClient(cfg)
	defer closeWebSocket()
	rtt, err := verifyLatency(ctx, cfg, wsClient, target)
	if err != nil && target.Transport == wire.TransportWebTransport && cfg.LatencyTransport == "auto" {
		if target, err = latencyTargetOver(cfg, targets, wire.TransportWebSocket); err != nil {
			return err
		}
		rtt, err = verifyLatency(ctx, cfg, wsClient, target)
	}
	if err != nil {
		return err
	}
	if target.Transport == wire.TransportWebTransport {
		if err := validatePingInterval(cfg.PingInterval); err != nil {
			return err
		}
	}
	prepared.LatencyTarget, prepared.WarmRTT = target, rtt
	return nil
}

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
	teardown      context.Context
}

func adaptiveWarmup(base, rtt time.Duration) time.Duration {
	const slowStartRTTs = 10
	return min(max(slowStartRTTs*rtt, base), 4*time.Second)
}

func (r *runner) measureDirection(ctx context.Context, dir Direction, gate *stageGate) error {
	if dir == Down {
		return r.measureDownload(ctx, gate)
	}
	return r.measureUpload(ctx, gate)
}

const stageReadyTimeout = 10 * time.Second

type stageGate struct {
	reportReady   func()
	boundaryStart time.Time
	cancel        context.CancelCauseFunc
	start         chan struct{}
}

func (r *runner) endpoint(path string) (string, error) {
	return httpEndpoint(r.target.Origin, path)
}

func selectTarget(cfg Config, pf wire.Preflight) (*wire.ThroughputTarget, error) {
	return firstMatch(cfg.ThroughputTransport, wire.TransportFetchStream, wire.TransportWebTransport,
		func(mechanism string) (*wire.ThroughputTarget, error) {
			return throughputTargetOver(cfg, pf, mechanism)
		})
}

func throughputTargetOver(cfg Config, pf wire.Preflight, mechanism string) (*wire.ThroughputTarget, error) {
	return pickTarget("throughput", pf.Capabilities.ThroughputTargets, cfg.ThroughputTarget, cfg.BaseURL,
		func(t *wire.ThroughputTarget) (string, string, bool) {
			protocolFits := cfg.ThroughputTarget != "auto" || cfg.ThroughputProtocol == "auto" ||
				t.Protocol == "negotiated" || t.Protocol == cfg.ThroughputProtocol
			return t.ID, t.Origin, t.Transport == mechanism && protocolFits
		})
}

func selectLatencyTarget(cfg Config, targets []wire.LatencyTarget) (*wire.LatencyTarget, error) {
	return firstMatch(cfg.LatencyTransport, wire.TransportWebTransport, wire.TransportWebSocket,
		func(mechanism string) (*wire.LatencyTarget, error) { return latencyTargetOver(cfg, targets, mechanism) })
}

func latencyTargetOver(cfg Config, targets []wire.LatencyTarget, mechanism string) (*wire.LatencyTarget, error) {
	return pickTarget("latency", targets, cfg.LatencyTarget, cfg.BaseURL,
		func(t *wire.LatencyTarget) (string, string, bool) { return t.ID, t.Origin, t.Transport == mechanism })
}

func firstMatch[T any](transport, preferred, fallback string, over func(string) (*T, error)) (*T, error) {
	order := []string{preferred, fallback}
	if transport != "auto" {
		order = []string{transport}
	}
	var firstErr error
	for _, mechanism := range order {
		t, err := over(mechanism)
		if err == nil {
			return t, nil
		}
		if firstErr == nil {
			firstErr = err
		}
	}
	return nil, firstErr
}

func pickTarget[T any](
	kind string,
	targets []T,
	selection, base string,
	eligible func(*T) (id, origin string, ok bool),
) (*T, error) {
	var candidates []*T
	for i := range targets {
		t := &targets[i]
		id, o, ok := eligible(t)
		switch {
		case !ok:
			continue
		case selection == "auto" && origin.Equal(o, base),
			selection != "auto" && (id == selection || origin.Equal(o, selection)):
			return t, nil
		}
		candidates = append(candidates, t)
	}
	switch {
	case selection == "auto" && len(candidates) == 1:
		return candidates[0], nil
	case selection == "auto" && len(candidates) > 1:
		return nil, fmt.Errorf("several %s targets are available; select an origin", kind)
	}
	return nil, fmt.Errorf("%s target %q unavailable", kind, selection)
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

func protocolClient(cfg Config, protocol string) (*http.Client, func()) {
	if protocol == "http3" {
		tr := &http3.Transport{
			TLSClientConfig:    &tls.Config{InsecureSkipVerify: cfg.InsecureSkipTLSVerify}, //nolint:gosec
			QUICConfig:         transport.NewQUICConfig(),
			DisableCompression: true,
		}
		return authenticatedClient(cfg, tr), func() { _ = tr.Close() }
	}
	tr := baseTransport(cfg)
	if protocol != "negotiated" {
		p := &http.Protocols{}
		p.SetHTTP1(protocol == "http1")
		p.SetHTTP2(protocol == "http2")
		tr.Protocols = p
	}
	return authenticatedClient(cfg, tr), tr.CloseIdleConnections
}
