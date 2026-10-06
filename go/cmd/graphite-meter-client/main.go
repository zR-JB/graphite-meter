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
	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/term"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
	"github.com/zR-JB/graphite-meter/go/internal/legal"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func main() {
	cfg := goclient.DefaultConfig()
	var showVersion, showLegal, report bool
	flag.StringVar(&cfg.BaseURL, "url", cfg.BaseURL, "origin of the operator server catalogue")
	flag.Func("server", fmt.Sprintf("selected catalogue ID (repeat up to %d times; omission uses operator defaults)",
		wire.MaxSelectedServers), func(id string) error {
		if id == "" || len(cfg.ServerIDs) >= wire.MaxSelectedServers || slices.Contains(cfg.ServerIDs, id) {
			return fmt.Errorf("select one to %d different server IDs", wire.MaxSelectedServers)
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
	flag.Func("stages", "comma-separated stages: latency (ping), download (down), upload (up), bidirectional "+
		"(bidi) (default latency,download,upload)", func(raw string) (err error) {
		cfg.Stages, err = parseStages(raw)
		return err
	})
	flag.DurationVar(&cfg.Warmup, "warmup", cfg.Warmup, "per-stage warmup duration")
	flag.DurationVar(&cfg.LatencyDuration, "latency-duration", cfg.LatencyDuration, "latency measurement duration")
	flag.DurationVar(&cfg.DownloadDuration, "download-duration", cfg.DownloadDuration, "download measurement duration")
	flag.DurationVar(&cfg.UploadDuration, "upload-duration", cfg.UploadDuration, "upload measurement duration")
	flag.DurationVar(&cfg.BidirectionalDuration, "bidirectional-duration", cfg.BidirectionalDuration,
		"bidirectional measurement duration")
	flag.IntVar(&cfg.TransferStreams.AutomaticMax, "auto-streams", cfg.TransferStreams.AutomaticMax,
		"maximum H1 streams per direction")
	flag.IntVar(&cfg.TransferStreams.Forced, "streams", cfg.TransferStreams.Forced,
		fmt.Sprintf("force exact streams per server and direction (0 = automatic; at most %d)", goclient.MaxStreams))
	cadence := fmt.Sprintf("reply-driven, fast, medium, slow, or a duration from %v to %v", goclient.PingFast,
		goclient.MaxPingInterval)
	flag.Func("ping", "idle latency cadence (default reply-driven): "+cadence, parsePing(&cfg.PingInterval))
	flag.Func("loaded-ping", "loaded latency cadence (default medium): "+cadence, parsePing(&cfg.LoadedPingInterval))
	flag.BoolVar(&cfg.LoadedLatency, "loaded-latency", cfg.LoadedLatency,
		"measure latency while transfer stages are loaded")
	flag.BoolVar(&cfg.InsecureSkipTLSVerify, "insecure", false, "skip TLS certificate verification")
	flag.BoolVar(&showVersion, "version", false, "print version and exit")
	flag.BoolVar(&report, "report", false, "run once without the interface and print the final report "+
		"(automatic when stdout is not a terminal)")
	flag.BoolVar(&showLegal, "legal", false, "print the licences of the bundled software and exit")
	flag.Parse()

	if showLegal {
		fmt.Print(string(legal.TUIReport()))
		return
	}
	if showVersion {
		fmt.Println("graphite-meter-client " + goclient.Version)
		return
	}
	if flag.NArg() > 0 {
		fail(2, fmt.Errorf("unexpected argument %q", flag.Arg(0)))
	}
	if err := cfg.Validate(); err != nil {
		fail(2, err)
	}

	var caught atomic.Value
	signals := make(chan os.Signal, 1)
	signal.Notify(signals, os.Interrupt, syscall.SIGTERM)
	m := newModel(cfg)
	if report || !term.IsTerminal(os.Stdout.Fd()) {
		onSignal(signals, &caught, m.controller.CancelRun)
		if m = runHeadless(m); m.run == nil {
			fmt.Fprintln(os.Stderr, "graphite-meter-client: "+m.notice)
		}
	} else {
		program := tea.NewProgram(m, tea.WithFPS(fps), tea.WithoutSignalHandler())
		go func() {
			for caughtSignal := range signals {
				caught.Store(caughtSignal)
				program.Send(interruptMsg{})
			}
		}()
		final, err := program.Run()
		m.controller.Close()
		if err != nil {
			fail(1, err)
		}
		m = final.(model)
	}
	if report := m.finalReport(); report != "" {
		lipgloss.Println(report)
	}
	os.Exit(exitStatus(m, caught.Load()))
}

func onSignal(signals chan os.Signal, caught *atomic.Value, react func()) {
	go func() {
		caught.Store(<-signals)
		signal.Stop(signals)
		react()
	}()
}

func exitStatus(m model, caught any) int {
	interrupted := m.interrupted || caught != nil && (m.running() || m.last == goclient.OutcomeStopped)
	switch {
	case interrupted && caught == syscall.SIGTERM:
		return 143
	case interrupted:
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
		m.width, m.st = w, newStyles(lipgloss.HasDarkBackground(os.Stdin, os.Stdout))
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
	return m
}

func fail(code int, err error) {
	fmt.Fprintf(os.Stderr, "graphite-meter-client: %v\n", err)
	os.Exit(code)
}

func parseStages(raw string) (goclient.StageSet, error) {
	var s goclient.StageSet
	for part := range strings.SplitSeq(raw, ",") {
		switch name := strings.TrimSpace(strings.ToLower(part)); name {
		case "latency", "ping":
			s.Latency = true
		case "download", "down":
			s.Download = true
		case "upload", "up":
			s.Upload = true
		case "bidirectional", "bidi":
			s.Bidirectional = true
		case "":
		default:
			return s, fmt.Errorf("unknown stage %q: use latency, download, upload, or bidirectional", name)
		}
	}
	return s, nil
}

func parsePing(interval *time.Duration) func(string) error {
	return func(raw string) error {
		name := strings.TrimSpace(raw)
		if i := slices.IndexFunc(cadences, func(c cadence) bool { return strings.EqualFold(c.key, name) }); i >= 0 {
			*interval = cadences[i].interval
			return nil
		}
		d, err := time.ParseDuration(name)
		if err != nil {
			return errors.New("use reply-driven, fast, medium, slow, or a duration such as 400ms")
		}
		*interval = d
		return nil
	}
}
