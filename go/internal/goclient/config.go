package goclient

import (
	"cmp"
	"errors"
	"fmt"
	"slices"
	"strconv"
	"strings"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

type StageSet struct {
	Latency       bool
	Download      bool
	Upload        bool
	Bidirectional bool
}

type StagePlan struct {
	Name       Stage
	Duration   time.Duration
	Directions []Direction
}

func (c Config) Plan() []StagePlan {
	var plan []StagePlan
	add := func(enabled bool, name Stage, duration time.Duration, directions ...Direction) {
		if enabled {
			plan = append(plan, StagePlan{name, duration, directions})
		}
	}
	add(c.Stages.Latency, StageLatency, c.LatencyDuration)
	add(c.Stages.Download, StageDownload, c.DownloadDuration, Down)
	add(c.Stages.Upload, StageUpload, c.UploadDuration, Up)
	add(c.Stages.Bidirectional, StageBidirectional, c.BidirectionalDuration, Down, Up)
	return plan
}

func (c Config) needsCheckpoint() bool { return c.Stages.Upload || c.Stages.Bidirectional }

// PreparationKey holds every setting a prepared run depends on; durations do not.
type PreparationKey struct {
	base, servers, throughputTarget, throughputProtocol, throughputTransport string
	latencyTarget, latencyTransport                                          string
	ping, loadedPing                                                         time.Duration
	insecure, latency, checkpoint                                            bool
	streams                                                                  TransferStreamPolicy
}

func (c Config) PreparationKey() PreparationKey {
	c = c.normalized()
	canonical, _ := wire.CanonicalOrigin(c.BaseURL)
	return PreparationKey{
		base:                cmp.Or(canonical, c.BaseURL),
		servers:             strings.Join(slices.Sorted(slices.Values(c.ServerIDs)), "\n"),
		throughputTarget:    c.ThroughputTarget,
		throughputProtocol:  c.ThroughputProtocol,
		throughputTransport: c.ThroughputTransport,
		latencyTarget:       c.LatencyTarget,
		latencyTransport:    c.LatencyTransport,
		ping:                c.PingInterval,
		loadedPing:          c.LoadedPingInterval,
		insecure:            c.InsecureSkipTLSVerify,
		latency:             c.needsLatency(),
		checkpoint:          c.needsCheckpoint(),
		streams:             c.TransferStreams,
	}
}

func (c Config) needsLatency() bool {
	return c.Stages.Latency || (c.LoadedLatency && (c.Stages.Download || c.Stages.Upload || c.Stages.Bidirectional))
}

type TransferStreamPolicy struct {
	AutomaticMax int
	Forced       int
}

const PingReplyDriven time.Duration = -1

const (
	PingFast   = 80 * time.Millisecond
	PingMedium = 250 * time.Millisecond
	PingSlow   = 600 * time.Millisecond
)

const (
	MaxTransferStreams = 128

	transferBytesPerStream = 64 << 30
	maxIdleConnsPerHost    = 256
	responseHeaderTimeout  = 10 * time.Second
	expectContinueTimeout  = time.Second
)

var multiplexedStreams = map[string]byDirection[int]{
	"http2": {down: 1, up: 4},
	"http3": {down: 1, up: 1},
}

// Lanes is the number of streams one server opens per direction on a path.
func (p TransferStreamPolicy) Lanes(protocol, transport string) (down, up int) {
	switch {
	case transport == wire.TransportWebTransport:
		n := 1
		if p.Forced > 0 {
			n = min(p.Forced, wire.WTMaxStreams)
		}
		return n, n
	case p.Forced > 0:
		return p.Forced, p.Forced
	}
	if lanes, ok := multiplexedStreams[protocol]; ok {
		return lanes.down, lanes.up
	}
	return p.AutomaticMax, p.AutomaticMax
}

const MaxPingInterval = wire.WTIdleBound / 2

func validatePingInterval(c Config) error {
	if max(c.PingInterval, c.LoadedPingInterval) > MaxPingInterval {
		return fmt.Errorf("ping interval must be at most %v, half the server's %v WebTransport idle bound",
			MaxPingInterval, wire.WTIdleBound)
	}
	return nil
}

type DurationBound struct{ Min, Max time.Duration }

// The stage minimum leaves room above the 800 ms of evidence every headline needs.
var (
	WarmupBound = DurationBound{0, 4 * time.Second}
	StageBound  = DurationBound{time.Second, 5 * time.Minute}
)

func (b DurationBound) Check(d time.Duration) error {
	if d < b.Min || d > b.Max {
		seconds := func(d time.Duration) string { return strconv.FormatFloat(d.Seconds(), 'f', -1, 64) + " s" }
		return fmt.Errorf("must be from %s to %s", seconds(b.Min), seconds(b.Max))
	}
	return nil
}

// Validate checks every user setting before a run; Run itself only needs checkPaths.
func (c Config) Validate() error {
	if len(c.Plan()) == 0 {
		return errors.New("select at least one stage: latency, download, upload or bidirectional")
	}
	if err := WarmupBound.Check(c.Warmup); err != nil {
		return fmt.Errorf("warmup %w", err)
	}
	for _, stage := range []StagePlan{{Name: StageLatency, Duration: c.LatencyDuration},
		{Name: StageDownload, Duration: c.DownloadDuration}, {Name: StageUpload, Duration: c.UploadDuration},
		{Name: StageBidirectional, Duration: c.BidirectionalDuration}} {
		if err := StageBound.Check(stage.Duration); err != nil {
			return fmt.Errorf("%s duration %w", stage.Name, err)
		}
	}
	for _, interval := range []time.Duration{c.PingInterval, c.LoadedPingInterval} {
		if interval != PingReplyDriven && interval < PingFast {
			return fmt.Errorf("ping cadence must be reply-driven or at least %v", PingFast)
		}
	}
	if streams := c.TransferStreams; streams.Forced < 0 || streams.Forced > MaxTransferStreams ||
		streams.AutomaticMax < 1 || streams.AutomaticMax > MaxTransferStreams {
		return fmt.Errorf("streams must be from 1 to %d", MaxTransferStreams)
	}
	return c.normalized().checkPaths()
}

func (c Config) checkPaths() error {
	fetch, ws, wt := wire.TransportFetchStream, wire.TransportWebSocket, wire.TransportWebTransport
	switch {
	case !slices.Contains([]string{"auto", "http1", "http2", "http3"}, c.ThroughputProtocol):
		return fmt.Errorf("invalid throughput protocol %q: use auto, http1, http2, or http3", c.ThroughputProtocol)
	case !slices.Contains([]string{"auto", fetch, wt}, c.ThroughputTransport):
		return fmt.Errorf("invalid throughput transport %q: use auto, %s, or %s", c.ThroughputTransport, fetch, wt)
	case !slices.Contains([]string{"auto", ws, wt}, c.LatencyTransport):
		return fmt.Errorf("invalid latency transport %q: use auto, %s, or %s", c.LatencyTransport, ws, wt)
	case c.LatencyTransport == wire.TransportWebTransport:
		return validatePingInterval(c)
	}
	return nil
}

type Config struct {
	BaseURL               string
	ServerIDs             []string
	ThroughputTarget      string
	ThroughputProtocol    string
	ThroughputTransport   string
	LatencyTarget         string
	LatencyTransport      string
	Stages                StageSet
	Warmup                time.Duration
	LatencyDuration       time.Duration
	DownloadDuration      time.Duration
	UploadDuration        time.Duration
	BidirectionalDuration time.Duration
	TransferStreams       TransferStreamPolicy
	PingInterval          time.Duration
	LoadedPingInterval    time.Duration
	LoadedLatency         bool
	InsecureSkipTLSVerify bool
}

func DefaultConfig() Config {
	return Config{
		BaseURL:               "http://127.0.0.1:7246",
		ThroughputTarget:      "auto",
		ThroughputProtocol:    "auto",
		ThroughputTransport:   "auto",
		LatencyTarget:         "auto",
		LatencyTransport:      "auto",
		Stages:                StageSet{Latency: true, Download: true, Upload: true},
		Warmup:                800 * time.Millisecond,
		LatencyDuration:       4 * time.Second,
		DownloadDuration:      10 * time.Second,
		UploadDuration:        10 * time.Second,
		BidirectionalDuration: 10 * time.Second,
		TransferStreams:       TransferStreamPolicy{AutomaticMax: 6},
		PingInterval:          PingReplyDriven,
		LoadedPingInterval:    PingMedium,
		LoadedLatency:         true,
	}
}

func (c Config) normalized() Config {
	d := DefaultConfig()
	c.BaseURL = cmp.Or(c.BaseURL, d.BaseURL)
	c.ThroughputTarget = cmp.Or(c.ThroughputTarget, d.ThroughputTarget)
	c.ThroughputProtocol = cmp.Or(c.ThroughputProtocol, d.ThroughputProtocol)
	c.ThroughputTransport = cmp.Or(c.ThroughputTransport, d.ThroughputTransport)
	c.LatencyTarget = cmp.Or(c.LatencyTarget, d.LatencyTarget)
	c.LatencyTransport = cmp.Or(c.LatencyTransport, d.LatencyTransport)
	c.Warmup = max(c.Warmup, 0)
	c.LatencyDuration = positive(c.LatencyDuration, d.LatencyDuration)
	c.DownloadDuration = positive(c.DownloadDuration, d.DownloadDuration)
	c.UploadDuration = positive(c.UploadDuration, d.UploadDuration)
	c.BidirectionalDuration = positive(c.BidirectionalDuration, d.BidirectionalDuration)
	c.TransferStreams.AutomaticMax = min(
		positive(c.TransferStreams.AutomaticMax, d.TransferStreams.AutomaticMax),
		MaxTransferStreams,
	)
	c.TransferStreams.Forced = min(max(c.TransferStreams.Forced, 0), MaxTransferStreams)
	if c.PingInterval != PingReplyDriven {
		c.PingInterval = positive(c.PingInterval, PingMedium)
	}
	if c.LoadedPingInterval != PingReplyDriven {
		c.LoadedPingInterval = positive(c.LoadedPingInterval, d.LoadedPingInterval)
	}
	return c
}

func positive[T int | time.Duration](value, fallback T) T {
	if value > 0 {
		return value
	}
	return fallback
}

func planRunStreams(cfg Config, servers []PreparedServer) (map[string]byDirection[int], error) {
	plan := map[string]byDirection[int]{}
	var total byDirection[int]
	for _, server := range servers {
		target := server.Connection.ThroughputTarget
		down, up := cfg.TransferStreams.Lanes(target.Protocol, target.Transport)
		lanes := byDirection[int]{down, up}
		plan[server.Server.ID] = lanes
		total.down += lanes.down
		total.up += lanes.up
	}
	if total.down > MaxTransferStreams || total.up > MaxTransferStreams {
		return nil, fmt.Errorf("the run exceeds %d streams per direction; reduce forced streams", MaxTransferStreams)
	}
	return plan, nil
}
