package goclient

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"net/http"
	"slices"
	"sync"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

type PreparedServer struct {
	Server     wire.ServerEntry
	Connection *PreparedConnection
	Err        error
	config     Config
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

// FreshFor reports whether a ready run was prepared for these settings recently enough to start.
func (p *PreparedRun) FreshFor(cfg Config) bool {
	return p.Ready() && p.key == cfg.PreparationKey() && time.Since(p.VerifiedAt) <= PreparationFreshness
}

func getCatalog(ctx context.Context, cfg Config) (wire.ServerCatalog, error) {
	tr := baseTransport(cfg)
	defer tr.CloseIdleConnections()
	hc := authenticatedClient(cfg, tr)
	// A catalogue is an authority boundary; a redirect cannot replace its operator.
	hc.CheckRedirect = func(*http.Request, []*http.Request) error {
		return errors.New("server catalogue must not redirect")
	}
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
	previous []wire.ServerEntry,
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
	cfg.grant = grants[base]
	catalog, err := getCatalog(ctx, cfg)
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
		for _, old := range previous {
			if old.ID == server.ID && old.URL != server.URL {
				return prepared, fmt.Errorf("%s changed origin; review and apply the server selection", server.Name)
			}
		}
		own := cfg
		own.BaseURL, own.server, own.grant = server.URL, new(server), grants[server.URL]
		prepared.Servers = append(prepared.Servers, PreparedServer{Server: server, config: own})
	}
	var work sync.WaitGroup
	for i := range prepared.Servers {
		work.Go(func() {
			server := &prepared.Servers[i]
			server.Connection, server.Err = prepare(ctx, server.config)
			if server.Err == nil && cfg.needsCheckpoint() && !server.Connection.Preflight.Capabilities.UploadCheckpoint {
				server.Err = errors.New("receiver checkpoint support is required; upgrade this measurement server")
			}
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
	if _, err := planRunStreams(cfg, prepared.Servers); err != nil {
		return prepared, err
	}
	return prepared, nil
}
