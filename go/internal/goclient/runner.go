package goclient

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"net/http"
	"net/url"
	"sync"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/origin"
	"github.com/zR-JB/graphite-meter/go/internal/route"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

type PreparedConnection struct {
	WarmRTT          time.Duration
	Preflight        wire.Preflight
	ThroughputTarget wire.ThroughputTarget
	LatencyTarget    *wire.LatencyTarget
}

type PreparationError struct {
	Preflight wire.Preflight
	Err       error
}

func (e *PreparationError) Error() string { return e.Err.Error() }
func (e *PreparationError) Unwrap() error { return e.Err }

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

// prepare checks one server's paths; server, when known, limits the targets it may advertise.
func prepare(ctx context.Context, cfg Config, server *wire.ServerEntry, cred *credential) (*PreparedConnection, error) {
	cfg = cfg.normalized()
	if err := cfg.checkPaths(); err != nil {
		return nil, err
	}
	base, err := url.Parse(cfg.BaseURL)
	if err != nil {
		return nil, err
	}
	if _, err := cred.authorize(base); err != nil {
		return nil, errors.New("authenticated operation requires verified HTTPS -url")
	}
	discoveryTransport := baseTransport(cred.insecure)
	defer discoveryTransport.CloseIdleConnections()
	pf, err := getPreflight(ctx, authenticatedClient(*cred, discoveryTransport), cfg.BaseURL)
	if err != nil {
		return nil, err
	}
	if server != nil {
		if err := server.ValidateDiscovery(pf); err != nil {
			return nil, &PreparationError{Preflight: pf, Err: err}
		}
	}
	if cfg.needsCheckpoint() && !pf.Capabilities.UploadCheckpoint {
		err := errors.New("receiver checkpoint support is required; upgrade this measurement server")
		return nil, &PreparationError{Preflight: pf, Err: err}
	}
	cred.reach(cfg.BaseURL, pf)
	branches, cancel := context.WithCancel(ctx)
	defer cancel()
	prepared := &PreparedConnection{Preflight: pf}
	var throughputErr, latencyErr error
	var work sync.WaitGroup
	work.Go(func() {
		if throughputErr = prepareThroughput(branches, cfg, *cred, prepared); throughputErr != nil {
			cancel()
		}
	})
	if cfg.needsLatency() {
		work.Go(func() {
			if latencyErr = prepareLatency(branches, cfg, *cred, prepared); latencyErr != nil {
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
	return prepared, nil
}

func prepareThroughput(ctx context.Context, cfg Config, cred credential, prepared *PreparedConnection) error {
	pf := prepared.Preflight
	selected, err := selectTarget(cfg, pf)
	if err != nil {
		return err
	}
	if selected.Transport == wire.TransportWebTransport {
		if err := verifyThroughputWebTransport(ctx, cred, selected); err != nil {
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
	transfer, closeTransfer := protocolClient(cred, target.Protocol)
	defer closeTransfer()
	clientProtocol, err := getJSONProbe(ctx, transfer, target.Origin, route.Probe)
	if err != nil {
		return err
	}
	if target.Protocol == "negotiated" {
		target.Protocol = protocolFromEvidence(clientProtocol)
	}
	prepared.ThroughputTarget = target
	return nil
}

func prepareLatency(ctx context.Context, cfg Config, cred credential, prepared *PreparedConnection) error {
	targets := prepared.Preflight.Capabilities.LatencyTargets
	target, err := selectLatencyTarget(cfg, targets)
	if err != nil {
		return err
	}
	wsClient, closeWebSocket := websocketClient(cred)
	defer closeWebSocket()
	rtt, err := verifyLatency(ctx, cred, wsClient, target)
	if err != nil && target.Transport == wire.TransportWebTransport && cfg.LatencyTransport == "auto" {
		if target, err = latencyTargetOver(cfg, targets, wire.TransportWebSocket); err != nil {
			return err
		}
		rtt, err = verifyLatency(ctx, cred, wsClient, target)
	}
	if err != nil {
		return err
	}
	if target.Transport == wire.TransportWebTransport {
		if err := validatePingInterval(cfg); err != nil {
			return err
		}
	}
	prepared.LatencyTarget, prepared.WarmRTT = target, rtt
	return nil
}

type runner struct {
	coordinated   *participantCounters
	cfg           Config
	cred          credential
	streams       byDirection[int]
	http          *http.Client
	websocketHTTP *http.Client
	uploadHTTP    *http.Client
	target        *wire.ThroughputTarget
	latencyTarget *wire.LatencyTarget
	emit          func(Event)
	idleRTT       time.Duration
	teardown      context.Context
}

func adaptiveWarmup(base, rtt time.Duration) time.Duration {
	const slowStartRTTs = 10
	return min(max(slowStartRTTs*rtt, base), WarmupBound.Max)
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
