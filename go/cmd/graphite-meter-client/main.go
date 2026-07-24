// Command graphite-meter-client is a native Bubble Tea speedtest client for the
// Graphite Meter server.
package main

import (
	"flag"
	"fmt"
	"os"
	"strings"
	"time"

	tea "github.com/charmbracelet/bubbletea"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
)

func main() {
	cfg := goclient.DefaultConfig()
	var stages string
	var ping string
	var showVersion bool
	flag.StringVar(&cfg.BaseURL, "url", cfg.BaseURL, "server base URL")
	flag.StringVar(&cfg.ThroughputTarget, "throughput-origin", cfg.ThroughputTarget, "throughput origin from discovery, or auto")
	flag.StringVar(&cfg.ThroughputProtocol, "throughput-protocol", cfg.ThroughputProtocol, "protocol for a negotiated throughput origin: auto, http1, http2, or http3")
	flag.StringVar(&cfg.ThroughputTransport, "throughput-transport", cfg.ThroughputTransport, "throughput transport: auto, fetch-stream, or webtransport")
	flag.StringVar(&cfg.LatencyTarget, "latency-origin", cfg.LatencyTarget, "latency origin from discovery, or auto")
	flag.StringVar(&cfg.LatencyTransport, "latency-transport", cfg.LatencyTransport, "latency transport: auto, websocket, or webtransport")
	flag.StringVar(&stages, "stages", "latency,download,upload", "comma-separated stages: latency,download,upload,bidirectional")
	flag.DurationVar(&cfg.Warmup, "warmup", cfg.Warmup, "per-stage warmup duration")
	flag.DurationVar(&cfg.LatencyDuration, "latency-duration", cfg.LatencyDuration, "latency measurement duration")
	flag.DurationVar(&cfg.DownloadDuration, "download-duration", cfg.DownloadDuration, "download measurement duration")
	flag.DurationVar(&cfg.UploadDuration, "upload-duration", cfg.UploadDuration, "upload measurement duration")
	flag.DurationVar(&cfg.BidirectionalDuration, "bidirectional-duration", cfg.BidirectionalDuration, "bidirectional measurement duration")
	flag.IntVar(&cfg.TransferStreams.AutomaticMax, "auto-streams", cfg.TransferStreams.AutomaticMax, "maximum automatic HTTP/1 streams per direction")
	flag.IntVar(&cfg.TransferStreams.Forced, "streams", cfg.TransferStreams.Forced, "force streams per active direction (0 = automatic)")
	flag.StringVar(&ping, "ping", "medium", "ping cadence: instant, medium, slow, or a duration")
	flag.BoolVar(&cfg.LoadedLatency, "loaded-latency", cfg.LoadedLatency, "measure latency while transfer stages are loaded")
	flag.BoolVar(&cfg.InsecureSkipTLSVerify, "insecure", false, "skip TLS certificate verification")
	flag.BoolVar(&showVersion, "version", false, "print version and exit")
	flag.Parse()

	if showVersion {
		fmt.Println("graphite-meter-client " + goclient.Version)
		return
	}

	cfg.Stages = parseStages(stages)
	cfg.PingInterval = parsePing(ping)

	// No mouse reporting: the terminal keeps its own selection, so the screen
	// stays copyable with the mouse or with a keyboard selection.
	p := tea.NewProgram(newModel(cfg), tea.WithFPS(30), tea.WithAltScreen())
	final, err := p.Run()
	if err != nil {
		fmt.Fprintf(os.Stderr, "graphite-meter-client: %v\n", err)
		os.Exit(1)
	}
	if m, ok := final.(model); ok {
		if report := m.finalReport(); report != "" {
			fmt.Println(report)
		}
	}
}

func parseStages(raw string) goclient.StageSet {
	var s goclient.StageSet
	for _, part := range strings.Split(raw, ",") {
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

func parsePing(raw string) time.Duration {
	switch strings.ToLower(strings.TrimSpace(raw)) {
	case "instant":
		return 80 * time.Millisecond
	case "slow":
		return 600 * time.Millisecond
	case "medium", "":
		return 250 * time.Millisecond
	default:
		d, err := time.ParseDuration(raw)
		if err != nil || d <= 0 {
			return 250 * time.Millisecond
		}
		return d
	}
}
