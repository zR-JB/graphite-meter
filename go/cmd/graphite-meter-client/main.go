// Command graphite-meter-client is a native terminal speedtest client for the Graphite Meter server.
package main

import (
	"errors"
	"flag"
	"fmt"
	"os"
	"os/signal"
	"slices"
	"strings"
	"sync/atomic"
	"syscall"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/term"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
	"github.com/zR-JB/graphite-meter/go/internal/legal"
)

func main() {
	if slices.Contains(os.Args[1:], "--legal") {
		fmt.Print(string(legal.TUIReport()))
		return
	}

	cfg := goclient.DefaultConfig()
	var stages, ping, loadedPing string
	var showVersion, report bool
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
		fmt.Sprintf("force exact streams per server and direction (0 = automatic; %d per direction across the run)",
			goclient.MaxTransferStreams))
	cadence := "reply-driven, fast, medium, slow, or a duration (up to " + goclient.MaxPingInterval.String() +
		" over the WebTransport latency path)"
	flag.StringVar(&ping, "ping", "", "idle ping cadence (default reply-driven): "+cadence)
	flag.StringVar(&loadedPing, "loaded-ping", "", "loaded ping cadence (default medium): "+cadence)
	flag.BoolVar(&cfg.LoadedLatency, "loaded-latency", cfg.LoadedLatency,
		"measure latency while transfer stages are loaded")
	flag.BoolVar(&cfg.InsecureSkipTLSVerify, "insecure", false, "skip TLS certificate verification")
	flag.BoolVar(&showVersion, "version", false, "print version and exit")
	flag.BoolVar(&report, "report", false, "run once without the interface and print the final report "+
		"(automatic when stdout is not a terminal)")
	flag.Parse()

	if showVersion {
		fmt.Println("graphite-meter-client " + goclient.Version)
		return
	}
	if flag.NArg() > 0 {
		fail(2, fmt.Errorf("unexpected argument %q", flag.Arg(0)))
	}
	cfg.Stages = parseStages(stages)
	for name, raw := range map[string]string{"-ping": ping, "-loaded-ping": loadedPing} {
		interval, err := parsePing(raw)
		switch {
		case raw == "":
		case err != nil:
			fail(2, fmt.Errorf("%s: %w", name, err))
		case name == "-ping":
			cfg.PingInterval = interval
		default:
			cfg.LoadedPingInterval = interval
		}
	}
	if err := cfg.Validate(); err != nil {
		fail(2, err)
	}

	var caught atomic.Value
	signals := make(chan os.Signal, 1)
	signal.Notify(signals, os.Interrupt, syscall.SIGTERM)
	m := newModel(cfg)
	if report || !term.IsTerminal(os.Stdout.Fd()) {
		go func() {
			caught.Store(<-signals)
			m.controller.CancelRun()
		}()
		m = runHeadless(m)
	} else {
		program := tea.NewProgram(m, tea.WithFPS(30), tea.WithoutSignalHandler())
		go func() {
			caught.Store(<-signals)
			program.Quit()
		}()
		final, err := program.Run()
		m.controller.Close()
		if err != nil {
			fail(1, err)
		}
		m = final.(model)
	}
	if report := m.finalReport(); report != "" {
		fmt.Println(report)
	}
	os.Exit(exitStatus(m, caught.Load()))
}

func exitStatus(m model, caught any) int {
	switch {
	case caught == syscall.SIGTERM:
		return 143
	case caught != nil || m.interrupted:
		return 130
	case m.last == "" || m.last == goclient.OutcomeComplete:
		return 0
	}
	return 1
}

func runHeadless(m model) model {
	defer m.controller.Close()
	m.width = 100
	if w, _, err := term.GetSize(os.Stdout.Fd()); err == nil && w > 0 {
		m.width = w
	}
	next, _ := m.startRun()
	m = next.(model)
	for m.running() {
		msg, ok := waitEvents(m.runSeq, m.events)().(eventsMsg)
		if !ok {
			break
		}
		for _, e := range msg.events {
			if e.Kind == goclient.EventStage && e.Phase == goclient.PhaseMeasuring {
				fmt.Fprintf(os.Stderr, "%s…\n", stageLabels[e.Stage])
			}
		}
		next, _ = m.Update(msg)
		m = next.(model)
	}
	if m.run == nil {
		fail(1, errors.New("sign-in required; run graphite-meter-client in a terminal to sign in"))
	}
	return m
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
	name := strings.TrimSpace(raw)
	if i := slices.IndexFunc(cadences, func(c cadence) bool { return strings.EqualFold(c.key, name) }); i >= 0 {
		return cadences[i].interval, nil
	}
	d, err := time.ParseDuration(name)
	if err != nil {
		return 0, errors.New("use reply-driven, fast, medium, slow, or a duration such as 400ms")
	}
	return d, nil
}
