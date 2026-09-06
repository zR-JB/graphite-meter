package goclient

import (
	"context"
	"errors"
	"fmt"
	"maps"
	"net/http"
	"slices"
	"sync"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// PreparedServer keeps transport evidence and credentials under one catalogue identity.
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
	return p != nil && p.Err == nil && len(p.Servers) > 0 && !slices.ContainsFunc(p.Servers, func(s PreparedServer) bool { return s.Err != nil || s.Connection == nil })
}

func (p *PreparedRun) SelectedIDs() []string {
	ids := make([]string, len(p.Servers))
	for i, s := range p.Servers {
		ids[i] = s.Server.ID
	}
	return ids
}

func (p *PreparedRun) FreshFor(cfg Config) bool {
	if !p.Ready() || p.configKey != selectionPreparationKey(cfg) || len(cfg.ServerIDs) > 0 && (len(cfg.ServerIDs) != len(p.Servers) || slices.ContainsFunc(p.Servers, func(s PreparedServer) bool { return !slices.Contains(cfg.ServerIDs, s.Server.ID) })) {
		return false
	}
	return !slices.ContainsFunc(p.Servers, func(s PreparedServer) bool {
		return !s.Connection.FreshFor(s.config) || (cfg.Stages.Upload || cfg.Stages.Bidirectional) && !s.Connection.Preflight.Capabilities.UploadCheckpoint
	})
}

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
	hc.CheckRedirect = func(*http.Request, []*http.Request) error { return errors.New("server catalogue must not redirect") }
	target, err := httpEndpoint(cfg.BaseURL, "/servers")
	if err != nil {
		return wire.ServerCatalog{}, err
	}
	var catalog wire.ServerCatalog
	if _, err := (jsonHTTPClient{hc}).requestJSON(ctx, http.MethodGet, target, nil, http.Header{"Cache-Control": {"no-store"}}, &catalog, httpStatusError("server catalogue")); err != nil {
		return catalog, err
	}
	if err := catalog.Validate(); err != nil {
		return catalog, err
	}
	catalog = catalog.Resolve(cfg.BaseURL)
	return catalog, catalog.Validate()
}

func prepareRun(ctx context.Context, cfg Config, previous []wire.ServerEntry, grants map[string]string) (result *PreparedRun, resultErr error) {
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
	if token := grants[base]; token != "" {
		cfg.AuthOrigin = base
		cfg.AuthToken = token
	}
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
		return prepared, errors.New("explicit origin overrides require a single selected server; use Automatic origins for simultaneous tests")
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
		own.BaseURL = server.URL
		own.server = new(server)
		own.AuthOrigin = server.URL
		own.AuthToken = grants[server.URL]
		if server.URL == base && own.AuthToken == "" {
			own.AuthToken = cfg.authToken()
		}
		prepared.Servers = append(prepared.Servers, PreparedServer{Server: server, config: own})
	}
	var work sync.WaitGroup
	for i := range prepared.Servers {
		work.Go(func() {
			server := &prepared.Servers[i]
			server.Connection, server.Err = Prepare(ctx, server.config)
			if server.Err == nil && (cfg.Stages.Upload || cfg.Stages.Bidirectional) && !server.Connection.Preflight.Capabilities.UploadCheckpoint {
				server.Err = errors.New("receiver checkpoint support is required; upgrade this measurement server")
			}
		})
	}
	work.Wait()
	for i := range prepared.Servers {
		server := &prepared.Servers[i]
		if server.Connection == nil {
			continue
		}
		metadata := server.Connection.Preflight.Server
		if metadata.Name != "" {
			server.Server.Name = metadata.Name
		}
		server.Server.Location = metadata.Location
		for j := range prepared.Catalog.Servers {
			if prepared.Catalog.Servers[j].ID == server.Server.ID {
				prepared.Catalog.Servers[j] = server.Server
			}
		}
	}
	var failures []error
	var best time.Duration
	for _, server := range prepared.Servers {
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

func (p *Preparation) PrepareRun() (*PreparedRun, error) {
	ctx, done, err := p.begin(preparationTimeout)
	if err != nil {
		return nil, err
	}
	defer done()
	p.owner.mu.Lock()
	previous := slices.Clone(p.owner.selection)
	grants := maps.Clone(p.owner.grants)
	p.owner.mu.Unlock()
	prepared, err := prepareRun(ctx, p.cfg, previous, grants)
	p.owner.mu.Lock()
	defer p.owner.mu.Unlock()
	if prepared != nil && p.ctx.Err() == nil {
		p.owner.catalog = new(prepared.Catalog)
		if prepared.Ready() {
			p.owner.selection = nil
			for _, server := range prepared.Servers {
				p.owner.selection = append(p.owner.selection, server.Server)
			}
		}
	}
	return prepared, err
}

// SelectServers acknowledges the identities displayed by the most recently loaded catalogue.
func (c *Controller) SelectServers(ids []string) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.catalog == nil {
		return errors.New("server catalogue is unavailable")
	}
	if err := c.catalog.ValidateSelection(ids); err != nil {
		return err
	}
	c.selection = nil
	for _, server := range c.catalog.Servers {
		if slices.Contains(ids, server.ID) {
			c.selection = append(c.selection, server)
		}
	}
	return nil
}

func (p *Preparation) BeginServerAuthorization(id, authURL string) (*PendingAuthorization, error) {
	if err := p.ctx.Err(); err != nil {
		return nil, err
	}
	cfg := p.cfg
	if id != "" {
		found := false
		p.owner.mu.Lock()
		if p.owner.catalog != nil {
			for _, server := range p.owner.catalog.Servers {
				if server.ID == id {
					cfg.BaseURL = server.URL
					found = true
					break
				}
			}
		}
		p.owner.mu.Unlock()
		if !found {
			return nil, errors.New("server is no longer in the catalogue")
		}
	}
	return BeginAuthorization(cfg, authURL)
}

// AcceptAuthorization keeps native grants in memory, indexed by their exact issuer origin.
func (c *Controller) AcceptAuthorization(origin, token string) error {
	canonical, err := wire.CanonicalOrigin(origin)
	if err != nil || canonical != origin {
		return errors.New("invalid authorization origin")
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	if token == "" || len(token) > 8192 {
		return errors.New("invalid authorization grant")
	}
	if _, exists := c.grants[origin]; !exists && len(c.grants) >= wire.MaxCatalogServers {
		return errors.New("too many authorized servers; restart the client to clear unused grants")
	}
	c.grants[origin] = token
	return nil
}
