package goclient

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/url"
	"strings"

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
	return pf, nil
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
