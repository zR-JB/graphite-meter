// Command graphite-meter-client is a native Bubble Tea speedtest client for the
// Graphite Meter server.
package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"strings"
	"time"

	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
)

func main() {
	cfg := goclient.DefaultConfig()
	var stages string
	var ping string
	flag.StringVar(&cfg.BaseURL, "url", cfg.BaseURL, "server base URL")
	flag.StringVar(&stages, "stages", "latency,download,upload", "comma-separated stages: latency,download,upload,bidirectional")
	flag.DurationVar(&cfg.Warmup, "warmup", cfg.Warmup, "per-stage warmup duration")
	flag.DurationVar(&cfg.LatencyDuration, "latency-duration", cfg.LatencyDuration, "latency measurement duration")
	flag.DurationVar(&cfg.DownloadDuration, "download-duration", cfg.DownloadDuration, "download measurement duration")
	flag.DurationVar(&cfg.UploadDuration, "upload-duration", cfg.UploadDuration, "upload measurement duration")
	flag.DurationVar(&cfg.BidirectionalDuration, "bidirectional-duration", cfg.BidirectionalDuration, "bidirectional measurement duration")
	flag.IntVar(&cfg.ParallelStreams, "streams", cfg.ParallelStreams, "parallel transfer streams")
	flag.StringVar(&ping, "ping", "medium", "ping cadence: instant, medium, slow, or a duration")
	flag.BoolVar(&cfg.LoadedLatency, "loaded-latency", cfg.LoadedLatency, "measure latency while transfer stages are loaded")
	flag.BoolVar(&cfg.InsecureSkipTLSVerify, "insecure", false, "skip TLS certificate verification")
	flag.Parse()

	cfg.Stages = parseStages(stages)
	cfg.PingInterval = parsePing(ping)

	events := make(chan goclient.Event, 256)
	done := make(chan error, 1)
	ctx, cancel := context.WithCancel(context.Background())
	go func() {
		done <- goclient.Run(ctx, cfg, func(e goclient.Event) {
			select {
			case events <- e:
			case <-ctx.Done():
			}
		})
		close(events)
	}()

	p := tea.NewProgram(newModel(cfg, events, done, cancel), tea.WithAltScreen())
	if _, err := p.Run(); err != nil {
		fmt.Fprintf(os.Stderr, "graphite-meter-client: %v\n", err)
		os.Exit(1)
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

type eventMsg goclient.Event
type doneMsg struct{ err error }

type model struct {
	cfg      goclient.Config
	events   <-chan goclient.Event
	done     <-chan error
	cancel   context.CancelFunc
	width    int
	height   int
	stage    string
	status   string
	server   string
	err      error
	complete bool

	rates   map[goclient.Direction]goclient.ThroughputSample
	peaks   map[goclient.Direction]float64
	results []goclient.Result
	latency goclient.LatencySample
}

func newModel(cfg goclient.Config, events <-chan goclient.Event, done <-chan error, cancel context.CancelFunc) model {
	return model{
		cfg:    cfg,
		events: events,
		done:   done,
		cancel: cancel,
		status: "starting",
		rates:  map[goclient.Direction]goclient.ThroughputSample{},
		peaks:  map[goclient.Direction]float64{},
	}
}

func (m model) Init() tea.Cmd {
	return tea.Batch(waitEvent(m.events), waitDone(m.done))
}

func waitEvent(events <-chan goclient.Event) tea.Cmd {
	return func() tea.Msg {
		e, ok := <-events
		if !ok {
			return nil
		}
		return eventMsg(e)
	}
}

func waitDone(done <-chan error) tea.Cmd {
	return func() tea.Msg {
		return doneMsg{err: <-done}
	}
}

func (m model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.WindowSizeMsg:
		m.width = msg.Width
		m.height = msg.Height
		return m, nil
	case tea.KeyMsg:
		switch msg.String() {
		case "q", "ctrl+c", "esc":
			m.cancel()
			return m, tea.Quit
		}
	case eventMsg:
		e := goclient.Event(msg)
		m.apply(e)
		if !m.complete && m.err == nil {
			return m, waitEvent(m.events)
		}
	case doneMsg:
		if msg.err != nil && !strings.Contains(msg.err.Error(), "context canceled") {
			m.err = msg.err
			m.status = "error"
		}
		m.complete = true
		return m, nil
	}
	return m, nil
}

func (m *model) apply(e goclient.Event) {
	switch e.Kind {
	case goclient.EventPreflight:
		if e.Preflight != nil {
			m.server = fmt.Sprintf("%s %s:%d %s", e.Preflight.Server.Name, e.Preflight.Server.Host, e.Preflight.Server.Port, e.Preflight.Server.Location)
			m.status = "connected"
		}
	case goclient.EventStage:
		m.stage = e.Stage
		m.status = e.Message
	case goclient.EventThroughput:
		m.rates[e.Direction] = e.Throughput
		if e.Throughput.BytesPerSec > m.peaks[e.Direction] {
			m.peaks[e.Direction] = e.Throughput.BytesPerSec
		}
	case goclient.EventLatency:
		m.latency = e.Latency
	case goclient.EventResult:
		if e.Result != nil {
			m.results = append(m.results, *e.Result)
		}
	case goclient.EventComplete:
		m.status = "complete"
		m.complete = true
	case goclient.EventError:
		m.err = e.Err
		m.status = "error"
	}
}

var (
	titleStyle  = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("230")).Background(lipgloss.Color("62")).Padding(0, 1)
	panelStyle  = lipgloss.NewStyle().Border(lipgloss.RoundedBorder()).BorderForeground(lipgloss.Color("240")).Padding(1, 2)
	labelStyle  = lipgloss.NewStyle().Foreground(lipgloss.Color("245"))
	valueStyle  = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("86"))
	mutedStyle  = lipgloss.NewStyle().Foreground(lipgloss.Color("244"))
	errorStyle  = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("203"))
	accentStyle = lipgloss.NewStyle().Foreground(lipgloss.Color("111"))
)

func (m model) View() string {
	w := m.width
	if w < 72 {
		w = 72
	}
	inner := w - 8
	if inner > 112 {
		inner = 112
	}
	var b strings.Builder
	b.WriteString(titleStyle.Render("Graphite Meter Go Client"))
	b.WriteString("\n\n")
	if m.err != nil {
		b.WriteString(errorStyle.Render(m.err.Error()))
		b.WriteString("\n\n")
	}
	b.WriteString(panelStyle.Width(inner).Render(m.summaryView(inner - 4)))
	b.WriteString("\n\n")
	b.WriteString(panelStyle.Width(inner).Render(m.liveView(inner - 4)))
	if len(m.results) > 0 {
		b.WriteString("\n\n")
		b.WriteString(panelStyle.Width(inner).Render(m.resultsView(inner - 4)))
	}
	b.WriteString("\n\n")
	b.WriteString(mutedStyle.Render("q quits"))
	return b.String()
}

func (m model) summaryView(w int) string {
	server := m.server
	if server == "" {
		server = "probing " + m.cfg.BaseURL
	}
	lines := []string{
		labelStyle.Render("Target ") + valueStyle.Render(server),
		labelStyle.Render("Stage  ") + valueStyle.Render(emptyDash(m.stage)) + mutedStyle.Render(" / "+emptyDash(m.status)),
		labelStyle.Render("Streams ") + valueStyle.Render(fmt.Sprintf("%d", m.cfg.ParallelStreams)) + mutedStyle.Render("  warmup "+m.cfg.Warmup.String()+"  ping "+m.cfg.PingInterval.String()),
	}
	return lipgloss.JoinVertical(lipgloss.Left, lines...)
}

func (m model) liveView(w int) string {
	down := m.rates[goclient.Down]
	up := m.rates[goclient.Up]
	lines := []string{
		accentStyle.Render("Live"),
		rateLine("download", down.BytesPerSec, m.peaks[goclient.Down], w),
		rateLine("upload  ", up.BytesPerSec, m.peaks[goclient.Up], w),
		latencyLine(m.latency),
	}
	return lipgloss.JoinVertical(lipgloss.Left, lines...)
}

func (m model) resultsView(w int) string {
	lines := []string{accentStyle.Render("Results")}
	for _, r := range m.results {
		if r.Latency.Count > 0 || r.Stage == "latency" {
			lines = append(lines, fmt.Sprintf("%-14s latency p50 %s  p95 %s  jitter %s  loss %.1f%%",
				r.Stage, fmtMs(r.Latency.P50), fmtMs(r.Latency.P95), fmtMs(r.Latency.Jitter), r.Latency.Loss*100))
			continue
		}
		auth := ""
		if r.ServerAuth {
			auth = " server-clock"
		}
		lines = append(lines, fmt.Sprintf("%-14s %-4s avg %s  peak %s  total %s%s",
			r.Stage, r.Direction, fmtRate(r.MeanBps), fmtRate(r.PeakBps), fmtBytes(r.TotalBytes), auth))
	}
	return lipgloss.JoinVertical(lipgloss.Left, lines...)
}

func rateLine(name string, bps, peak float64, w int) string {
	barW := w - 34
	if barW < 12 {
		barW = 12
	}
	if peak < bps {
		peak = bps
	}
	fill := 0
	if peak > 0 {
		fill = int((bps / peak) * float64(barW))
	}
	if fill > barW {
		fill = barW
	}
	bar := strings.Repeat("=", fill) + strings.Repeat("-", barW-fill)
	return fmt.Sprintf("%s %s %12s", labelStyle.Render(name), accentStyle.Render(bar), valueStyle.Render(fmtRate(bps)))
}

func latencyLine(s goclient.LatencySample) string {
	if s.Lost {
		return labelStyle.Render("latency ") + errorStyle.Render("lost")
	}
	if s.RTT <= 0 {
		return labelStyle.Render("latency ") + mutedStyle.Render("--")
	}
	load := ""
	if s.UnderLoad {
		load = " loaded"
	}
	return labelStyle.Render("latency ") + valueStyle.Render(fmtMs(s.RTT)) + mutedStyle.Render(load)
}

func emptyDash(s string) string {
	if s == "" {
		return "--"
	}
	return s
}

func fmtRate(bytesPerSec float64) string {
	bits := bytesPerSec * 8
	units := []string{"bit/s", "Kbit/s", "Mbit/s", "Gbit/s", "Tbit/s"}
	i := 0
	for bits >= 1000 && i < len(units)-1 {
		bits /= 1000
		i++
	}
	if i == 0 {
		return fmt.Sprintf("%.0f %s", bits, units[i])
	}
	return fmt.Sprintf("%.2f %s", bits, units[i])
}

func fmtBytes(n uint64) string {
	v := float64(n)
	units := []string{"B", "KB", "MB", "GB", "TB"}
	i := 0
	for v >= 1000 && i < len(units)-1 {
		v /= 1000
		i++
	}
	if i == 0 {
		return fmt.Sprintf("%d %s", n, units[i])
	}
	return fmt.Sprintf("%.2f %s", v, units[i])
}

func fmtMs(d time.Duration) string {
	if d <= 0 {
		return "--"
	}
	return fmt.Sprintf("%.2f ms", float64(d.Microseconds())/1000)
}
