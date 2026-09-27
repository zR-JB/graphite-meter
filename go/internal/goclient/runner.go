package goclient

import (
	"context"
	"net/http"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

type runner struct {
	coordinated   *participantCounters
	cfg           Config
	cred          credential
	streams       byDirection[int]
	http          *http.Client
	websocketHTTP *http.Client
	uploadHTTP    *http.Client
	target        *wire.ThroughputTarget
	latencyTarget *wire.LatencyTarget
	emit          func(Event)
	idleRTT       time.Duration
	teardown      context.Context
}

func adaptiveWarmup(base, rtt time.Duration) time.Duration {
	const slowStartRTTs = 10
	return min(max(slowStartRTTs*rtt, base), WarmupBound.Max)
}

const stageReadyTimeout = 10 * time.Second

type stageGate struct {
	reportReady   func()
	boundaryStart time.Time
	cancel        context.CancelCauseFunc
	start         chan struct{}
}

func (r *runner) endpoint(path string) (string, error) {
	return httpEndpoint(r.target.Origin, path)
}
