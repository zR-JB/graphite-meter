package goclient

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"net/http"
	"net/url"
	"slices"
	"sync"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/origin"
	"github.com/zR-JB/graphite-meter/go/internal/route"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

type PreparedServer struct {
	Server     wire.ServerEntry
	Connection *PreparedConnection
	Err        error
	credential credential
}

type PreparedRun struct {
	Err          error
	Catalog      wire.ServerCatalog
	Servers      []PreparedServer
	LatencyFocus string
	VerifiedAt   time.Time
	key          PreparationKey
}

const PreparationFreshness = 30 * time.Second

func (p *PreparedRun) Ready() bool {
	failed := func(s PreparedServer) bool { return s.Err != nil || s.Connection == nil }
	return p != nil && p.Err == nil && len(p.Servers) > 0 && !slices.ContainsFunc(p.Servers, failed)
}

func (p *PreparedRun) SelectedIDs() []string {
	ids := make([]string, len(p.Servers))
	for i, s := range p.Servers {
		ids[i] = s.Server.ID
	}
	return ids
}

func (p *PreparedRun) FreshFor(cfg Config) bool {
	return p.Ready() && p.key == cfg.PreparationKey() && time.Since(p.VerifiedAt) <= PreparationFreshness
}

func getCatalog(ctx context.Context, cfg Config, cred credential) (wire.ServerCatalog, error) {
	tr := baseTransport(cred.insecure)
	defer tr.CloseIdleConnections()
	hc := authenticatedClient(cred, tr)
	target, err := httpEndpoint(cfg.BaseURL, "/servers")
	if err != nil {
		return wire.ServerCatalog{}, err
	}
	var catalog wire.ServerCatalog
	if _, err := controlJSON(ctx, hc, http.MethodGet, target, "server catalogue", &catalog); err != nil {
		return catalog, err
	}
	if err := catalog.Validate(); err != nil {
		return catalog, err
	}
	catalog = catalog.Resolve(cfg.BaseURL)
	return catalog, catalog.Validate()
}

func prepareRun(
	ctx context.Context,
	cfg Config,
	previous *PreparedRun,
	grants map[string]string,
) (result *PreparedRun, resultErr error) {
	defer func() {
		if result != nil {
			result.Err = resultErr
		}
	}()
	cfg = cfg.normalized()
	verified := time.Now()
	base, err := wire.CanonicalOrigin(cfg.BaseURL)
	if err != nil {
		return nil, err
	}
	cfg.BaseURL = base
	credentialFor := func(origin string) credential {
		return credential{token: grants[origin], origins: []string{origin}, insecure: cfg.InsecureSkipTLSVerify}
	}
	catalog, err := getCatalog(ctx, cfg, credentialFor(base))
	if err != nil {
		return nil, err
	}
	ids := cfg.ServerIDs
	if len(ids) == 0 {
		ids = catalog.DefaultSelection
	}
	cfg.ServerIDs = ids
	prepared := &PreparedRun{Catalog: catalog, VerifiedAt: verified, key: cfg.PreparationKey()}
	if err := catalog.ValidateSelection(ids); err != nil {
		return prepared, err
	}
	if len(ids) > 1 && (cfg.ThroughputTarget != "auto" || cfg.LatencyTarget != "auto") {
		return prepared, errors.New("explicit origins need a single selected server; use Automatic origins for several")
	}
	for _, server := range catalog.Servers {
		if !slices.Contains(ids, server.ID) {
			continue
		}
		if previous.Ready() && slices.ContainsFunc(previous.Servers, func(old PreparedServer) bool {
			return old.Server.ID == server.ID && old.Server.URL != server.URL
		}) {
			return prepared, fmt.Errorf("%s changed origin; review the server selection and check again", server.Name)
		}
		own := PreparedServer{Server: server, credential: credentialFor(server.URL)}
		prepared.Servers = append(prepared.Servers, own)
	}
	var work sync.WaitGroup
	for i := range prepared.Servers {
		work.Go(func() {
			server := &prepared.Servers[i]
			own := cfg
			own.BaseURL = server.Server.URL
			server.Connection, server.Err = prepare(ctx, own, new(server.Server), &server.credential)
		})
	}
	work.Wait()
	var failures []error
	var best time.Duration
	for i := range prepared.Servers {
		server := &prepared.Servers[i]
		if server.Connection != nil {
			metadata := server.Connection.Preflight.Server
			server.Server.Name = cmp.Or(metadata.Name, server.Server.Name)
			server.Server.Location = metadata.Location
			same := func(s wire.ServerEntry) bool { return s.ID == server.Server.ID }
			if j := slices.IndexFunc(prepared.Catalog.Servers, same); j >= 0 {
				prepared.Catalog.Servers[j] = server.Server
			}
		}
		if server.Err != nil {
			failures = append(failures, fmt.Errorf("%s: %w", server.Server.Name, server.Err))
			continue
		}
		rtt := server.Connection.WarmRTT
		if prepared.LatencyFocus == "" || rtt > 0 && (best <= 0 || rtt < best) {
			prepared.LatencyFocus = server.Server.ID
			best = rtt
		}
	}
	if len(failures) > 0 {
		return prepared, errors.Join(failures...)
	}
	return prepared, nil
}

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
