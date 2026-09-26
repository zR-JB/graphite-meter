package endpoint

import (
	"crypto/rand"
	"encoding/json/v2"
	"errors"
	"log"
	"net"
	"net/http"
	"net/url"
	"slices"
	"strings"
	"sync"

	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/origin"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// Discovery serves /preflight, /servers and the page's connect policy, built once per request hostname.
type Discovery struct {
	cfg        *config.Config
	generation string // a per-process random tag

	mu    sync.Mutex
	hosts map[string]*hostDiscovery
}

type hostDiscovery struct {
	preflight  []byte
	servers    []byte
	serversErr error
	connect    []string // distinct cross-origin targets
	csp        string
}

// Request hostnames are untrusted: one that is not a valid host is read as localhost, and the cache is bounded.
const maxDiscoveryHosts = 64

func NewDiscovery(cfg *config.Config) *Discovery {
	return &Discovery{cfg: cfg, generation: rand.Text(), hosts: make(map[string]*hostDiscovery)}
}

func RequestHost(r *http.Request) string { return (&url.URL{Host: r.Host}).Hostname() }

func (d *Discovery) forHost(host string) *hostDiscovery {
	if _, err := wire.CanonicalOrigin("http://" + net.JoinHostPort(host, "1")); err != nil {
		host = "localhost"
	}
	d.mu.Lock()
	defer d.mu.Unlock()
	if h, ok := d.hosts[host]; ok {
		return h
	}
	for evicted := range d.hosts {
		if len(d.hosts) < maxDiscoveryHosts {
			break
		}
		delete(d.hosts, evicted)
	}
	h := d.build(host)
	d.hosts[host] = h
	return h
}

func (d *Discovery) ServePreflight(w http.ResponseWriter, r *http.Request) {
	noStoreJSON(w)
	_, _ = w.Write(d.forHost(RequestHost(r)).preflight)
}

func (d *Discovery) ServeServers(w http.ResponseWriter, r *http.Request) {
	h := d.forHost(RequestHost(r))
	if h.serversErr != nil {
		http.Error(w, "server catalogue unavailable", http.StatusInternalServerError)
		return
	}
	noStoreJSON(w)
	_, _ = w.Write(h.servers)
}

func (d *Discovery) ConnectOrigins(host string) []string { return d.forHost(host).connect }

func (d *Discovery) PagePolicy(host string) string { return d.forHost(host).csp }

func (d *Discovery) build(host string) *hostDiscovery {
	pf := d.preflightFor(host)
	h := &hostDiscovery{}
	h.preflight, _ = json.Marshal(pf) // the document is plain strings, slices and booleans
	seen := map[string]bool{"": true, ".": true}
	add := func(o string) {
		if !seen[o] {
			seen[o] = true
			h.connect = append(h.connect, o)
		}
	}
	for _, t := range pf.Capabilities.ThroughputTargets {
		add(t.Origin)
	}
	for _, t := range pf.Capabilities.LatencyTargets {
		add(t.Origin)
		add(websocketOrigin(t.Origin))
	}
	// Callers share the slice; clipping makes their appends copy.
	h.connect = slices.Clip(h.connect)
	if h.servers, h.serversErr = d.serversFor(h.connect); h.serversErr != nil {
		log.Printf("[gm:discovery] server catalogue for host %q: %v", host, h.serversErr)
	}
	sources := append([]string{"'self'"}, d.cfg.ServerCatalog.ConnectSources()...)
	for _, raw := range h.connect {
		parsed := strings.Replace(strings.Replace(raw, "wss://", "https://", 1), "ws://", "http://", 1)
		if _, err := wire.CanonicalOrigin(parsed); err == nil && wire.BrowserConnectSourceSupported(raw) {
			sources = append(sources, raw)
		}
	}
	h.csp = "frame-ancestors 'none'; base-uri 'none'; object-src 'none'; connect-src " + strings.Join(sources, " ")
	return h
}

func (d *Discovery) serversFor(connect []string) ([]byte, error) {
	c := d.cfg.ServerCatalog
	if len(c.Servers) == 0 {
		c = wire.SingletonCatalog()
	}
	c.Servers = slices.Clone(c.Servers)
	c.Servers[0].Name, c.Servers[0].Location = d.cfg.ServerName, d.cfg.ServerLocation
	c.Servers[0].AdditionalOrigins = slices.Clone(c.Servers[0].AdditionalOrigins)
	for _, origin := range connect {
		if strings.HasPrefix(origin, "http://") || strings.HasPrefix(origin, "https://") {
			c.Servers[0].AdditionalOrigins = append(c.Servers[0].AdditionalOrigins, origin)
		}
	}
	if err := c.Validate(); err != nil {
		return nil, err
	}
	data, err := json.Marshal(c)
	if err == nil && len(data) > 64<<10 {
		err = errors.New("published catalogue exceeds 64 KiB")
	}
	return data, err
}

func websocketOrigin(target string) string {
	if host, ok := strings.CutPrefix(target, "https://"); ok {
		return "wss://" + host
	}
	if host, ok := strings.CutPrefix(target, "http://"); ok {
		return "ws://" + host
	}
	return ""
}

// preflightFor lists the targets host's clients reach: each advertised native listener with its fixed
// protocol, then the proxied public origins, merging a fetch origin offered under different protocols.
func (d *Discovery) preflightFor(host string) wire.Preflight {
	throughput, latency := []wire.ThroughputTarget{}, []wire.LatencyTarget{}
	fetch := func(base, protocol string) {
		for i, t := range throughput {
			if t.Transport == wire.TransportFetchStream && origin.Equal(t.Origin, base) {
				if t.Protocol != protocol {
					throughput[i].Protocol = "negotiated"
				}
				return
			}
		}
		throughput = append(throughput, wire.ThroughputTarget{Origin: base, Transport: wire.TransportFetchStream,
			Protocol: protocol})
	}
	websocket := func(base string) {
		if !slices.ContainsFunc(latency, func(t wire.LatencyTarget) bool {
			return t.Transport == wire.TransportWebSocket && origin.Equal(t.Origin, base)
		}) {
			latency = append(latency, wire.LatencyTarget{Origin: base, Transport: wire.TransportWebSocket})
		}
	}
	cfg := d.cfg
	for _, n := range cfg.Natives() {
		if !cfg.NativeAdvertised(n.Name) {
			continue
		}
		base := nativeOrigin(n.Public, n.Scheme, host, n.Addr)
		fetch(base, n.Protocol)
		switch n.Protocol {
		case "http1":
			websocket(base)
		case "http3":
			throughput = append(throughput,
				wire.ThroughputTarget{Origin: base, Transport: wire.TransportWebTransport, Protocol: "http3"},
				wire.ThroughputTarget{Origin: base, Transport: wire.TransportWebTransportDatagram, Protocol: "http3"})
			latency = append(latency, wire.LatencyTarget{Origin: base, Transport: wire.TransportWebTransport})
		}
	}
	self := func(base string) string {
		if base == "self" {
			return "."
		}
		return base
	}
	for _, base := range slices.Concat(cfg.Public.Both, cfg.Public.Throughput) {
		fetch(self(base), "negotiated")
	}
	for _, base := range slices.Concat(cfg.Public.Both, cfg.Public.Latency) {
		websocket(self(base))
	}
	return wire.Preflight{Server: wire.ServerInfo{Name: cfg.ServerName, Location: cfg.ServerLocation},
		EngineVersion: cfg.EngineVersion, Generation: d.generation, Capabilities: wire.Capabilities{
			UploadCheckpoint: true, ThroughputTargets: throughput, LatencyTargets: latency}}
}

func nativeOrigin(public, scheme, host, addr string) string {
	if public != "" {
		return public
	}
	_, port, err := net.SplitHostPort(addr)
	if err != nil || port == "" {
		return scheme + "://" + host
	}
	return (&url.URL{Scheme: scheme, Host: net.JoinHostPort(host, port)}).String()
}
