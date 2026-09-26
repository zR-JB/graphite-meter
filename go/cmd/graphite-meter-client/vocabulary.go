package main

import (
	"cmp"
	"fmt"
	"slices"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/goclient"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

const missing = "—"

var stageLabels = map[goclient.Stage]string{
	goclient.StageLatency:       "Latency",
	goclient.StageDownload:      "Download",
	goclient.StageUpload:        "Upload",
	goclient.StageBidirectional: "Bidirectional",
}

func compactStage(stage goclient.Stage) string {
	if stage == goclient.StageBidirectional {
		return "Bi-dir"
	}
	return stageLabels[stage]
}

func populationLabel(stage goclient.Stage) string {
	if stage == goclient.StageLatency {
		return "Idle latency"
	}
	return "Loaded latency · " + stageLabels[stage]
}

func compactPopulation(stage goclient.Stage) string {
	return map[goclient.Stage]string{goclient.StageLatency: "Idle", goclient.StageDownload: "Loaded down",
		goclient.StageUpload: "Loaded up", goclient.StageBidirectional: "Loaded bi-dir"}[stage]
}

func directionLabel(r goclient.Result) string {
	if r.Stage != goclient.StageBidirectional {
		return stageLabels[r.Stage]
	}
	if r.Direction == goclient.Up {
		return "Bi-dir ↑"
	}
	return "Bi-dir ↓"
}

var outcomeLabels = map[goclient.Outcome]string{
	goclient.OutcomeComplete:   "Complete",
	goclient.OutcomePartial:    "Partial",
	goclient.OutcomeIncomplete: "Incomplete",
	goclient.OutcomeStopped:    "Stopped",
	goclient.OutcomeFailed:     "Failed",
}

var transportLabels = map[string]string{
	wire.TransportFetchStream:          "Fetch streams",
	wire.TransportWebSocket:            "WebSocket",
	wire.TransportWebTransport:         "WebTransport streams",
	wire.TransportWebTransportDatagram: "WebTransport datagrams",
}

func transportLabel(kind string, latency bool) string {
	if kind == wire.TransportWebTransport && latency {
		kind = wire.TransportWebTransportDatagram
	}
	return cmp.Or(transportLabels[kind], kind)
}

func protocolLabel(protocol string) string {
	labels := map[string]string{"auto": "Automatic", "http1": "HTTP/1.1", "http2": "HTTP/2", "http3": "HTTP/3",
		"negotiated": "Negotiated", "": missing}
	return cmp.Or(labels[protocol], protocol)
}

func connectionSummary(kind, protocol string, tls, latency bool) string {
	security := "clear"
	if tls {
		security = "TLS"
	}
	return transportLabel(kind, latency) + " · " + protocolLabel(protocol) + " · " + security
}

func streamsLabel(p goclient.TransferStreamPolicy, protocol, kind string) string {
	down, up := p.Lanes(protocol, kind)
	switch {
	case p.Forced > down:
		return fmt.Sprintf("Forced · %d per direction (capped from %d by the session)", down, p.Forced)
	case p.Forced > 0:
		return fmt.Sprintf("Forced · %d per direction", p.Forced)
	case kind == wire.TransportWebTransport:
		return "Automatic · 1 continuous stream per direction"
	case protocol == "http2" || protocol == "http3":
		return fmt.Sprintf("Automatic · %d download / %d upload", down, up)
	case protocol == "http1":
		return fmt.Sprintf("Automatic · up to %d per direction", down)
	}
	return "Automatic"
}

type cadence struct {
	key, label string
	interval   time.Duration
}

var cadences = []cadence{
	{"reply-driven", "Reply-driven", goclient.PingReplyDriven},
	{"fast", "Fast (80 ms)", goclient.PingFast},
	{"medium", "Medium (250 ms)", goclient.PingMedium},
	{"slow", "Slow (600 ms)", goclient.PingSlow},
}

func cadenceIndex(interval time.Duration) int {
	return slices.IndexFunc(cadences, func(c cadence) bool { return c.interval == interval })
}

func cadenceLabel(interval time.Duration) string {
	if i := cadenceIndex(interval); i >= 0 {
		return cadences[i].label
	}
	return "Custom (" + fmtSetting(interval) + ")"
}
