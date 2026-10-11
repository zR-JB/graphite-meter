package main

import (
	"strings"

	"github.com/zR-JB/graphite-meter/go/internal/goclient"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// wireMultiplier is the wire bytes each payload byte takes over target on IP version ipVersion: every layer's
// headers at a 1,500 B MTU, as the browser estimates them (client/src/lib/compensation.ts, its central estimate).
func wireMultiplier(target wire.ThroughputTarget, ipVersion int) float64 {
	const mtu, ethernet, record = 1500.0, 38.0, 16384.0
	quic := target.Transport != wire.TransportFetchStream || target.Protocol == "http3"
	http2 := !quic && target.Protocol == "http2"
	framing := 0.0
	switch {
	case quic:
		framing = 5
	case http2:
		framing = 9
	}
	application := 1 + framing/record
	if !quic && (http2 || strings.HasPrefix(target.Origin, "https:")) {
		application *= 1 + 22/record
	}
	ip := 20.0
	if ipVersion == 6 {
		ip = 40
	}
	// UDP with a QUIC short header (8 B connection ID, 2 B packet number) and AEAD tag, or TCP with timestamps.
	transport := 20.0 + 12
	if quic {
		transport = 8 + 1 + 8 + 2 + 16
	}
	return application * (mtu + ethernet) / (mtu - ip - transport)
}

// wireRate is a stage's wire rate in bytes per second: each server's measured rates over its own path.
func wireRate(details *goclient.RunDetails, stage goclient.Stage) (float64, bool) {
	if details == nil {
		return 0, false
	}
	total := 0.0
	for _, server := range details.Servers {
		rate := 0.0
		for _, result := range server.Results {
			if result.Stage == stage && result.Direction != "" && !result.Unavailable {
				rate += result.MeanBps
			}
		}
		total += rate * wireMultiplier(server.Throughput, server.IPVersion)
	}
	return total, total > 0
}
