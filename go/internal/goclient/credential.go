package goclient

import (
	"crypto/tls"
	"errors"
	"net"
	"net/http"
	"net/url"
	"slices"
	"strings"
	"time"

	"github.com/quic-go/quic-go/http3"

	"github.com/zR-JB/graphite-meter/go/internal/origin"
	"github.com/zR-JB/graphite-meter/go/internal/transport"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// credential is one server's grant, the origins it may reach and whether TLS is verified on the way.
type credential struct {
	token    string
	origins  []string
	insecure bool
}

var errGrantScope = errors.New("refusing to send authentication grant outside the server's verified HTTPS origins")

// authorize returns the header a request to u carries; a grant never crosses unverified TLS or another origin.
func (c credential) authorize(u *url.URL) (http.Header, error) {
	if c.token == "" {
		return nil, nil
	}
	here := u.Scheme + "://" + u.Host
	if c.insecure || u.Scheme != "https" || !slices.ContainsFunc(c.origins, func(o string) bool {
		return origin.Equal(o, here)
	}) {
		return nil, errGrantScope
	}
	return http.Header{"Authorization": {"Bearer " + c.token}}, nil
}

// reach extends the grant to advertised targets on the server's own hostname.
func (c *credential) reach(base string, pf wire.Preflight) {
	b, err := url.Parse(base)
	if err != nil {
		return
	}
	var targets []string
	for _, t := range pf.Capabilities.ThroughputTargets {
		targets = append(targets, t.Origin)
	}
	for _, t := range pf.Capabilities.LatencyTargets {
		targets = append(targets, t.Origin)
	}
	for _, target := range targets {
		if u, err := url.Parse(target); err == nil && strings.EqualFold(u.Hostname(), b.Hostname()) {
			c.origins = append(c.origins, target)
		}
	}
}

type authTransport struct {
	credential
	base http.RoundTripper
}

func (t authTransport) RoundTrip(r *http.Request) (*http.Response, error) {
	header, err := t.authorize(r.URL)
	if err != nil {
		return nil, err
	}
	if header != nil {
		r = r.Clone(r.Context())
		r.Header.Set("Authorization", header.Get("Authorization"))
	}
	return t.base.RoundTrip(r)
}

func authenticatedClient(c credential, base http.RoundTripper) *http.Client {
	return &http.Client{
		Transport: authTransport{c, base},
		CheckRedirect: func(*http.Request, []*http.Request) error {
			return errors.New("measurement and authentication endpoints must not redirect")
		},
	}
}

func baseTransport(insecure bool) *http.Transport {
	return &http.Transport{
		Proxy:                 http.ProxyFromEnvironment,
		DialContext:           (&net.Dialer{Timeout: 10 * time.Second, KeepAlive: 30 * time.Second}).DialContext,
		ForceAttemptHTTP2:     true,
		MaxIdleConns:          maxIdleConnsPerHost * 2,
		MaxIdleConnsPerHost:   maxIdleConnsPerHost,
		IdleConnTimeout:       90 * time.Second,
		ResponseHeaderTimeout: responseHeaderTimeout,
		ExpectContinueTimeout: expectContinueTimeout,
		TLSClientConfig:       &tls.Config{InsecureSkipVerify: insecure}, //nolint:gosec
		WriteBufferSize:       256 * 1024,
		ReadBufferSize:        256 * 1024,
		DisableCompression:    true,
		// The 4 MiB default stream window caps H2 downloads per RTT.
		HTTP2: &http.HTTP2Config{MaxReceiveBufferPerStream: 32 << 20, MaxReceiveBufferPerConnection: 64 << 20},
	}
}

func websocketClient(c credential) (*http.Client, func()) {
	tr := baseTransport(c.insecure)
	protocols := &http.Protocols{}
	protocols.SetHTTP1(true)
	tr.Protocols = protocols
	return authenticatedClient(c, tr), tr.CloseIdleConnections
}

func protocolClient(c credential, protocol string) (*http.Client, func()) {
	if protocol == "http3" {
		tr := &http3.Transport{
			TLSClientConfig:    &tls.Config{InsecureSkipVerify: c.insecure}, //nolint:gosec
			QUICConfig:         transport.NewQUICConfig(),
			DisableCompression: true,
		}
		return authenticatedClient(c, tr), func() { _ = tr.Close() }
	}
	tr := baseTransport(c.insecure)
	if protocol != "negotiated" {
		p := &http.Protocols{}
		p.SetHTTP1(protocol == "http1")
		p.SetHTTP2(protocol == "http2")
		tr.Protocols = p
	}
	return authenticatedClient(c, tr), tr.CloseIdleConnections
}
