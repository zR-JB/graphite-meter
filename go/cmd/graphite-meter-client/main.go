// Command graphite-meter-client is a native Bubble Tea speedtest client for the Graphite Meter server.
package main

import (
	"flag"
	"fmt"
	"os"
	"slices"
	"strings"
	"time"

	tea "github.com/charmbracelet/bubbletea"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
	"github.com/zR-JB/graphite-meter/go/internal/legal"
)

func main() {
	if slices.Contains(os.Args[1:], "--legal") {
		fmt.Print(string(legal.TUIReport()))
		return
	}

	cfg := goclient.DefaultConfig()
	var stages, ping string
	var showVersion bool
	flag.StringVar(&cfg.BaseURL, "url", cfg.BaseURL, "origin of the operator server catalogue")
	flag.Func("server", "selected catalogue ID (repeat up to four times; omission uses operator defaults)",
		func(id string) error {
			if id == "" || len(cfg.ServerIDs) >= 4 || slices.Contains(cfg.ServerIDs, id) {
				return fmt.Errorf("select one to four different server IDs")
			}
			cfg.ServerIDs = append(cfg.ServerIDs, id)
			return nil
		})
	flag.StringVar(&cfg.ThroughputTarget, "throughput-origin", cfg.ThroughputTarget,
		"throughput origin from discovery, or auto")
	flag.StringVar(&cfg.ThroughputProtocol, "throughput-protocol", cfg.ThroughputProtocol,
		"protocol for a negotiated throughput origin: auto, http1, http2, or http3")
	flag.StringVar(&cfg.ThroughputTransport, "throughput-transport", cfg.ThroughputTransport,
		"throughput transport: auto, fetch-stream, or webtransport")
	flag.StringVar(&cfg.LatencyTarget, "latency-origin", cfg.LatencyTarget, "latency origin from discovery, or auto")
	flag.StringVar(&cfg.LatencyTransport, "latency-transport", cfg.LatencyTransport,
		"latency transport: auto, websocket, or webtransport")
	flag.StringVar(&stages, "stages", "latency,download,upload",
		"comma-separated stages: latency,download,upload,bidirectional")
	flag.DurationVar(&cfg.Warmup, "warmup", cfg.Warmup, "per-stage warmup duration")
	flag.DurationVar(&cfg.LatencyDuration, "latency-duration", cfg.LatencyDuration, "latency measurement duration")
	flag.DurationVar(&cfg.DownloadDuration, "download-duration", cfg.DownloadDuration, "download measurement duration")
	flag.DurationVar(&cfg.UploadDuration, "upload-duration", cfg.UploadDuration, "upload measurement duration")
	flag.DurationVar(&cfg.BidirectionalDuration, "bidirectional-duration", cfg.BidirectionalDuration,
		"bidirectional measurement duration")
	flag.IntVar(&cfg.TransferStreams.AutomaticMax, "auto-streams", cfg.TransferStreams.AutomaticMax,
		"maximum H1 streams per direction")
	flag.IntVar(&cfg.TransferStreams.Forced, "streams", cfg.TransferStreams.Forced,
		"force exact streams per server and direction (0 = automatic; 128 per direction across the run)")
	flag.StringVar(&ping, "ping", "medium", "ping cadence: fast (80 ms), medium (250 ms), slow (600 ms), "+
		"or a duration (up to "+goclient.MaxPingInterval.String()+" over the WebTransport latency path)")
	flag.BoolVar(&cfg.LoadedLatency, "loaded-latency", cfg.LoadedLatency,
		"measure latency while transfer stages are loaded")
	flag.BoolVar(&cfg.InsecureSkipTLSVerify, "insecure", false, "skip TLS certificate verification")
	flag.BoolVar(&showVersion, "version", false, "print version and exit")
	flag.Parse()

	if showVersion {
		fmt.Println("graphite-meter-client " + goclient.Version)
		return
	}
	cfg.Stages = parseStages(stages)
	interval, err := parsePing(ping)
	if err != nil {
		fail(2, fmt.Errorf("-ping: %w", err))
	}
	cfg.PingInterval = interval
	if err := cfg.Validate(); err != nil {
		fail(2, err)
	}

	m := newModel(cfg)
	final, err := tea.NewProgram(m, tea.WithFPS(30), tea.WithAltScreen()).Run()
	m.controller.Close()
	if err != nil {
		fail(1, err)
	}
	if report := final.(model).finalReport(); report != "" {
		fmt.Println(report)
	}
}

func fail(code int, err error) {
	fmt.Fprintf(os.Stderr, "graphite-meter-client: %v\n", err)
	os.Exit(code)
}

func parseStages(raw string) goclient.StageSet {
	var s goclient.StageSet
	for part := range strings.SplitSeq(raw, ",") {
		switch strings.TrimSpace(strings.ToLower(part)) {
		case "latency", "ping":
			s.Latency = true
		case "download", "down":
			s.Download = true
		case "upload", "up":
			s.Upload = true
		case "bidirectional", "bidi":
			s.Bidirectional = true
		}
	}
	return s
}

func parsePing(raw string) (time.Duration, error) {
	name := strings.ToLower(strings.TrimSpace(raw))
	if name == "" {
		return 250 * time.Millisecond, nil
	}
	named := func(c cadence) bool { return strings.HasPrefix(strings.ToLower(c.label), name+" ") }
	if i := slices.IndexFunc(cadences, named); i >= 0 {
		return cadences[i].interval, nil
	}
	d, err := time.ParseDuration(name)
	if err != nil || d <= 0 {
		return 0, fmt.Errorf("use fast, medium, slow, or a positive duration such as 400ms")
	}
	return d, nil
}
