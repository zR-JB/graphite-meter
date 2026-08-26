package goclient

import (
	"context"
	"encoding/json/v2"
	"fmt"
	"net/http"
	"net/url"
	"strings"
	"time"

	"github.com/coder/websocket"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func getPreflight(ctx context.Context, hc *http.Client, base string) (wire.Preflight, error) {
	u, err := url.JoinPath(strings.TrimRight(base, "/"), "/preflight")
	if err != nil {
		return wire.Preflight{}, err
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, u, nil)
	if err != nil {
		return wire.Preflight{}, err
	}
	req.Header.Set("Cache-Control", "no-store")
	res, err := hc.Do(req)
	if err != nil {
		return wire.Preflight{}, err
	}
	defer res.Body.Close()
	if res.StatusCode != http.StatusOK {
		if err := authResponseError(res); err != nil {
			return wire.Preflight{}, err
		}
		return wire.Preflight{}, fmt.Errorf("preflight returned HTTP %d", res.StatusCode)
	}
	var pf wire.Preflight
	if err := json.UnmarshalRead(res.Body, &pf); err != nil {
		return wire.Preflight{}, err
	}
	baseOrigin, err := url.Parse(res.Request.URL.String())
	if err != nil {
		return wire.Preflight{}, err
	}
	baseOrigin.Path, baseOrigin.RawQuery, baseOrigin.Fragment = "", "", ""
	resolveSelfOrigins(&pf, baseOrigin.String())
	return pf, nil
}

// resolveSelfOrigins replaces the wire's "." self placeholder with the origin
// the preflight request resolves to, redirects included. A server behind a
// reverse proxy cannot know its own public origin.
func resolveSelfOrigins(pf *wire.Preflight, resolved string) {
	for i := range pf.Capabilities.ThroughputTargets {
		if pf.Capabilities.ThroughputTargets[i].Origin == "." {
			normalizeThroughputTarget(&pf.Capabilities.ThroughputTargets[i], resolved)
		}
	}
	for i := range pf.Capabilities.LatencyTargets {
		if pf.Capabilities.LatencyTargets[i].Origin == "." {
			normalizeLatencyTarget(&pf.Capabilities.LatencyTargets[i], resolved)
		}
	}
}

// The advertised transport survives normalization; only the origin is resolved.
func normalizeThroughputTarget(t *wire.ThroughputTarget, origin string) {
	t.ID, t.Origin, t.TLS, t.Routes = origin, strings.TrimRight(origin, "/"), strings.HasPrefix(origin, "https://"), wire.DefaultThroughputRoutes()
}
func normalizeLatencyTarget(t *wire.LatencyTarget, origin string) {
	t.ID, t.Origin, t.TLS, t.Routes = origin, strings.TrimRight(origin, "/"), strings.HasPrefix(origin, "https://"), wire.DefaultLatencyRoutes()
}

func getProbe(ctx context.Context, hc *http.Client, target *wire.ThroughputTarget) (wire.Probe, string, error) {
	u, err := httpEndpoint(target.Origin, target.Routes.Probe)
	if err != nil {
		return wire.Probe{}, "", err
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, u, nil)
	if err != nil {
		return wire.Probe{}, "", err
	}
	res, err := hc.Do(req)
	if err != nil {
		return wire.Probe{}, "", err
	}
	defer res.Body.Close()
	if res.StatusCode != http.StatusOK {
		if err := authResponseError(res); err != nil {
			return wire.Probe{}, "", err
		}
		return wire.Probe{}, "", fmt.Errorf("probe returned HTTP %d", res.StatusCode)
	}
	var p wire.Probe
	if err := json.UnmarshalRead(res.Body, &p); err != nil {
		return wire.Probe{}, "", err
	}
	return p, res.Proto, nil
}

func getLatencyProbe(ctx context.Context, hc *http.Client, target *wire.LatencyTarget) (wire.Probe, error) {
	u, err := httpEndpoint(target.Origin, target.Routes.Probe)
	if err != nil {
		return wire.Probe{}, err
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, u, nil)
	if err != nil {
		return wire.Probe{}, err
	}
	res, err := hc.Do(req)
	if err != nil {
		return wire.Probe{}, err
	}
	defer res.Body.Close()
	if res.StatusCode != http.StatusOK {
		if err := authResponseError(res); err != nil {
			return wire.Probe{}, err
		}
		return wire.Probe{}, fmt.Errorf("latency probe returned HTTP %d", res.StatusCode)
	}
	var p wire.Probe
	if err := json.UnmarshalRead(res.Body, &p); err != nil {
		return wire.Probe{}, err
	}
	return p, nil
}

func verifyLatencyWebSocket(ctx context.Context, hc *http.Client, target *wire.LatencyTarget) error {
	u, err := wsEndpoint(target.Origin, target.Routes.Ping)
	if err != nil {
		return err
	}
	verifyCtx, cancel := context.WithTimeout(ctx, 3*time.Second)
	defer cancel()
	conn, response, err := websocket.Dial(verifyCtx, u, &websocket.DialOptions{HTTPClient: hc, CompressionMode: websocket.CompressionDisabled})
	if err != nil {
		if authErr := authResponseError(response); authErr != nil {
			return authErr
		}
		return fmt.Errorf("latency WebSocket connection failed: %w", err)
	}
	defer conn.Close(websocket.StatusNormalClosure, "")
	if err := conn.Write(verifyCtx, websocket.MessageText, []byte(wire.Encode(wire.Frame{Op: wire.OpHI, Proto: "ws"}))); err != nil {
		return fmt.Errorf("latency WebSocket hello failed: %w", err)
	}
	_, message, err := conn.Read(verifyCtx)
	if err != nil {
		return fmt.Errorf("latency WebSocket readiness failed: %w", err)
	}
	frame, err := wire.Decode(string(message))
	if err != nil || frame.Op != wire.OpREADY {
		return fmt.Errorf("latency WebSocket did not become ready")
	}
	return nil
}

func httpEndpoint(base, path string) (string, error) {
	return url.JoinPath(strings.TrimRight(base, "/"), path)
}

func wsEndpoint(base, path string) (string, error) {
	u, err := httpEndpoint(base, path)
	if err != nil {
		return "", err
	}
	switch {
	case strings.HasPrefix(u, "https://"):
		return "wss://" + strings.TrimPrefix(u, "https://"), nil
	case strings.HasPrefix(u, "http://"):
		return "ws://" + strings.TrimPrefix(u, "http://"), nil
	default:
		return u, nil
	}
}
