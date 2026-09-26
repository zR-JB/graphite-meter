package goclient

import (
	"context"
	"maps"
	"net/http"
	"net/url"
	"strings"

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
			t.ID, t.Origin = origin, origin
		}
	}
	for i := range pf.Capabilities.LatencyTargets {
		if t := &pf.Capabilities.LatencyTargets[i]; t.Origin == "." {
			t.ID, t.Origin = origin, origin
		}
	}
	return pf, nil
}

func getJSONProbe(ctx context.Context, hc *http.Client, origin, path string) (string, error) {
	u, err := httpEndpoint(origin, path)
	if err != nil {
		return "", err
	}
	var p wire.Probe
	response, err := controlJSON(ctx, hc, http.MethodGet, u, "probe", &p)
	if err != nil {
		return "", err
	}
	return response.Proto, p.Validate()
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
