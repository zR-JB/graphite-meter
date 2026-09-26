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
	if _, err := planRunStreams(cfg, prepared.Servers); err != nil {
		return prepared, err
	}
	return prepared, nil
}
