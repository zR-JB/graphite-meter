package goclient

import (
	"context"
	"encoding/json"
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
		return wire.Preflight{}, fmt.Errorf("preflight returned HTTP %d", res.StatusCode)
	}
	var pf wire.Preflight
	if err := json.NewDecoder(res.Body).Decode(&pf); err != nil {
		return wire.Preflight{}, err
	}
	baseOrigin, err := url.Parse(res.Request.URL.String())
	if err != nil {
		return wire.Preflight{}, err
	}
	baseOrigin.Path, baseOrigin.RawQuery, baseOrigin.Fragment = "", "", ""
	for i := range pf.Capabilities.ThroughputTargets {
		if pf.Capabilities.ThroughputTargets[i].Origin == "." {
			normalizeThroughputTarget(&pf.Capabilities.ThroughputTargets[i], baseOrigin.String())
		}
	}
	for i := range pf.Capabilities.LatencyTargets {
		if pf.Capabilities.LatencyTargets[i].Origin == "." {
			normalizeLatencyTarget(&pf.Capabilities.LatencyTargets[i], baseOrigin.String())
		}
	}
	return pf, nil
}

func normalizeThroughputTarget(t *wire.ThroughputTarget, origin string) {
	t.ID, t.Origin, t.Transport, t.TLS, t.Routes = origin, strings.TrimRight(origin, "/"), "fetch-stream", strings.HasPrefix(origin, "https://"), wire.DefaultThroughputRoutes()
}
func normalizeLatencyTarget(t *wire.LatencyTarget, origin string) {
	t.ID, t.Origin, t.Transport, t.Protocol, t.TLS, t.Routes = origin, strings.TrimRight(origin, "/"), "websocket", "http1", strings.HasPrefix(origin, "https://"), wire.DefaultLatencyRoutes()
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
		return wire.Probe{}, "", fmt.Errorf("probe returned HTTP %d", res.StatusCode)
	}
	var p wire.Probe
	if err := json.NewDecoder(res.Body).Decode(&p); err != nil {
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
		return wire.Probe{}, fmt.Errorf("latency probe returned HTTP %d", res.StatusCode)
	}
	var p wire.Probe
	if err := json.NewDecoder(res.Body).Decode(&p); err != nil {
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
	conn, _, err := websocket.Dial(verifyCtx, u, &websocket.DialOptions{HTTPClient: hc, CompressionMode: websocket.CompressionDisabled})
	if err != nil {
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
