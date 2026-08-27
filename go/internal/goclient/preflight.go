package goclient

import (
	"context"
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
	var pf wire.Preflight
	response, err := (jsonHTTPClient{hc}).requestJSON(ctx, http.MethodGet, u, nil, http.Header{"Cache-Control": {"no-store"}}, &pf, httpStatusError("preflight"))
	if err != nil {
		return wire.Preflight{}, err
	}
	baseOrigin := response.Request.URL.Clone()
	baseOrigin.Path, baseOrigin.RawQuery, baseOrigin.Fragment = "", "", ""
	resolveSelfOrigins(&pf, baseOrigin.String())
	return pf, nil
}

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

func normalizeThroughputTarget(t *wire.ThroughputTarget, origin string) {
	t.ID, t.Origin, t.TLS, t.Routes = origin, strings.TrimRight(origin, "/"), strings.HasPrefix(origin, "https://"), wire.DefaultThroughputRoutes()
}
func normalizeLatencyTarget(t *wire.LatencyTarget, origin string) {
	t.ID, t.Origin, t.TLS, t.Routes = origin, strings.TrimRight(origin, "/"), strings.HasPrefix(origin, "https://"), wire.DefaultLatencyRoutes()
}

func getJSONProbe(ctx context.Context, hc *http.Client, origin, path, statusPrefix string) (wire.Probe, string, error) {
	u, err := httpEndpoint(origin, path)
	if err != nil {
		return wire.Probe{}, "", err
	}
	var p wire.Probe
	response, err := (jsonHTTPClient{hc}).requestJSON(ctx, http.MethodGet, u, nil, nil, &p, httpStatusError(statusPrefix))
	if err != nil {
		return wire.Probe{}, "", err
	}
	return p, response.Proto, nil
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

func endpointWithQuery(base string, query url.Values) (string, error) {
	u, err := url.Parse(base)
	if err != nil {
		return "", err
	}
	values := u.Query()
	for key, list := range query {
		if len(list) > 0 {
			values.Set(key, list[0])
		}
	}
	u.RawQuery = values.Encode()
	return u.String(), nil
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
