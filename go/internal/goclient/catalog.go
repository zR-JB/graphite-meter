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
	configKey    string
}

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
	if !p.Ready() || p.configKey != selectionPreparationKey(cfg) {
		return false
	}
	if ids := cfg.ServerIDs; len(ids) > 0 {
		if len(ids) != len(p.Servers) {
			return false
		}
		for _, s := range p.Servers {
			if !slices.Contains(ids, s.Server.ID) {
				return false
			}
		}
	}
	return !slices.ContainsFunc(p.Servers, func(s PreparedServer) bool {
		return !s.Connection.FreshFor(s.config) ||
			needsCheckpoint(cfg) && !s.Connection.Preflight.Capabilities.UploadCheckpoint
	})
}

func needsCheckpoint(cfg Config) bool { return cfg.Stages.Upload || cfg.Stages.Bidirectional }

func selectionPreparationKey(cfg Config) string {
	if canonical, err := wire.CanonicalOrigin(cfg.BaseURL); err == nil {
		cfg.BaseURL = canonical
	}
	return preparationKey(cfg.normalized())
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
	requestKey := selectionPreparationKey(cfg)
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
	prepared := &PreparedRun{Catalog: catalog, configKey: requestKey}
	ids := cfg.ServerIDs
	if len(ids) == 0 {
		ids = catalog.DefaultSelection
	}
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
			if server.Err == nil && needsCheckpoint(cfg) && !server.Connection.Preflight.Capabilities.UploadCheckpoint {
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
		rtt := server.Connection.PreflightRTT
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
