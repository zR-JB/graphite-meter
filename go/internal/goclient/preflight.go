package goclient

import (
	"context"
	"fmt"
	"maps"
	"net/http"
	"net/url"
	"strings"
	"time"

	"github.com/coder/websocket"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func getPreflight(ctx context.Context, hc *http.Client, base string) (wire.Preflight, error) {
	u, err := httpEndpoint(base, "/preflight")
	if err != nil {
		return wire.Preflight{}, err
	}
	var pf wire.Preflight
	response, err := controlJSON(ctx, hc, http.MethodGet, u, "preflight", &pf)
	if err != nil {
		return wire.Preflight{}, err
	}
	if err := pf.Validate(); err != nil {
		return wire.Preflight{}, err
	}
	self := response.Request.URL.Clone()
	self.Path, self.RawQuery, self.Fragment = "", "", ""
	origin := self.String()
	for i := range pf.Capabilities.ThroughputTargets {
		if t := &pf.Capabilities.ThroughputTargets[i]; t.Origin == "." {
			t.ID, t.Origin, t.TLS, t.Routes = origin, origin, self.Scheme == "https", wire.DefaultThroughputRoutes()
		}
	}
	for i := range pf.Capabilities.LatencyTargets {
		if t := &pf.Capabilities.LatencyTargets[i]; t.Origin == "." {
			t.ID, t.Origin, t.TLS, t.Routes = origin, origin, self.Scheme == "https", wire.DefaultLatencyRoutes()
		}
	}
	return pf, nil
}

func getJSONProbe(ctx context.Context, hc *http.Client, origin, path, what string) (wire.Probe, string, error) {
	u, err := httpEndpoint(origin, path)
	if err != nil {
		return wire.Probe{}, "", err
	}
	var p wire.Probe
	response, err := controlJSON(ctx, hc, http.MethodGet, u, what, &p)
	if err != nil {
		return wire.Probe{}, "", err
	}
	if err := p.Validate(); err != nil {
		return wire.Probe{}, "", err
	}
	return p, response.Proto, nil
}

func verifyLatencyWebSocket(ctx context.Context, hc *http.Client, target *wire.LatencyTarget) error {
	u, err := wsEndpoint(target.Origin, route.Ping)
	if err != nil {
		return err
	}
	verifyCtx, cancel := context.WithTimeout(ctx, 3*time.Second)
	defer cancel()
	conn, response, err := websocket.Dial(verifyCtx, u, &websocket.DialOptions{
		HTTPClient:      hc,
		CompressionMode: websocket.CompressionDisabled,
	})
	if err != nil {
		if authErr := authResponseError(response); authErr != nil {
			return authErr
		}
		return fmt.Errorf("latency WebSocket connection failed: %w", err)
	}
	defer conn.Close(websocket.StatusNormalClosure, "")
	if err := conn.Write(verifyCtx, websocket.MessageText, []byte(wire.EncodePing(0))); err != nil {
		return fmt.Errorf("latency WebSocket probe failed: %w", err)
	}
	for {
		_, message, err := conn.Read(verifyCtx)
		if err != nil {
			return fmt.Errorf("latency WebSocket readiness failed: %w", err)
		}
		if pong, err := wire.DecodePong(string(message)); err == nil && pong.ID == 0 {
			return nil
		}
	}
}

func httpEndpoint(base, path string) (string, error) {
	return url.JoinPath(strings.TrimRight(base, "/"), path)
}

func endpointWithQuery(base string, query url.Values) (string, error) {
	u, err := url.Parse(base)
	if err != nil {
		return "", err
	}
	values := u.Query()
	maps.Copy(values, query)
	u.RawQuery = values.Encode()
	return u.String(), nil
}

func wsEndpoint(base, path string) (string, error) {
	u, err := httpEndpoint(base, path)
	if err != nil {
		return "", err
	}
	if host, ok := strings.CutPrefix(u, "https://"); ok {
		return "wss://" + host, nil
	}
	if host, ok := strings.CutPrefix(u, "http://"); ok {
		return "ws://" + host, nil
	}
	return u, nil
}
