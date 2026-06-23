// Command graphite-meter-client is a native Bubble Tea speedtest client for the
// Graphite Meter server.
package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"strconv"
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

	p := tea.NewProgram(newModel(cfg))
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

type mode int

const (
	modeConfigure mode = iota
	modeRun
)

type section int

const (
	sectionServers section = iota
	sectionStages
	sectionTiming
	sectionNetwork
	sectionRun
	sectionCount
)

var sectionLabels = []string{"Servers", "Stages", "Timing", "Network", "Run"}

type editKind int

const (
	editNone editKind = iota
	editURL
	editDuration
	editInt
)

type editState struct {
	kind  editKind
	field string
	value string
}

type serverPreset struct {
	name string
	url  string
	note string
}

var serverPresets = []serverPreset{
	{name: "Local dev", url: "http://127.0.0.1:8080", note: "default HTTP listener"},
	{name: "Local TLS", url: "https://127.0.0.1:8443", note: "local HTTPS listener"},
	{name: "LAN host", url: "http://graphite-meter.local:8080", note: "mDNS or local DNS"},
}

type model struct {
	cfg    goclient.Config
	mode   mode
	width  int
	height int

	section section
	row     int
	edit    editState
	notice  string

	events <-chan goclient.Event
	done   <-chan error
	cancel context.CancelFunc

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

func newModel(cfg goclient.Config) model {
	return model{
		cfg:    cfg,
		mode:   modeConfigure,
		notice: "Choose a server, tune the profile, then press r to run.",
		rates:  map[goclient.Direction]goclient.ThroughputSample{},
		peaks:  map[goclient.Direction]float64{},
	}
}

func (m model) Init() tea.Cmd {
	return nil
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
		return m.handleKey(msg)
	case eventMsg:
		e := goclient.Event(msg)
		m.apply(e)
		if m.mode == modeRun && !m.complete && m.err == nil {
			return m, waitEvent(m.events)
		}
	case doneMsg:
		if msg.err != nil && !strings.Contains(msg.err.Error(), "context canceled") {
			m.err = msg.err
			m.status = "error"
		}
		if msg.err != nil && strings.Contains(msg.err.Error(), "context canceled") {
			m.status = "canceled"
		}
		m.complete = true
		return m, nil
	}
	return m, nil
}

func (m model) handleKey(msg tea.KeyMsg) (tea.Model, tea.Cmd) {
	if m.edit.kind != editNone {
		return m.handleEditKey(msg)
	}

	switch msg.String() {
	case "ctrl+c", "q":
		if m.cancel != nil {
			m.cancel()
		}
		return m, tea.Quit
	}

	if m.mode == modeRun {
		switch msg.String() {
		case "esc", "c":
			if !m.complete && m.cancel != nil {
				m.cancel()
				m.status = "canceling"
			}
		case "m", "left":
			if m.complete {
				m.mode = modeConfigure
				m.section = sectionRun
				m.row = 0
			}
		case "r":
			if m.complete {
				return m.startRun()
			}
		}
		return m, nil
	}

	switch msg.String() {
	case "tab", "right":
		m.section = (m.section + 1) % sectionCount
		m.row = clamp(m.row, 0, m.rowCount()-1)
	case "shift+tab", "left":
		m.section = (m.section + sectionCount - 1) % sectionCount
		m.row = clamp(m.row, 0, m.rowCount()-1)
	case "up", "k":
		m.row = clamp(m.row-1, 0, m.rowCount()-1)
	case "down", "j":
		m.row = clamp(m.row+1, 0, m.rowCount()-1)
	case "enter", " ":
		return m.activate()
	case "r":
		return m.startRun()
	case "esc":
		m.notice = "Configuration kept. Press q to quit or r to run."
	}
	return m, nil
}

func (m model) handleEditKey(msg tea.KeyMsg) (tea.Model, tea.Cmd) {
	switch msg.String() {
	case "ctrl+c":
		if m.cancel != nil {
			m.cancel()
		}
		return m, tea.Quit
	case "esc":
		m.notice = "Edit canceled."
		m.edit = editState{}
		return m, nil
	case "enter":
		m.commitEdit()
		return m, nil
	case "backspace", "ctrl+h":
		if len(m.edit.value) > 0 {
			m.edit.value = m.edit.value[:len(m.edit.value)-1]
		}
		return m, nil
	}
	if len(msg.Runes) > 0 {
		m.edit.value += string(msg.Runes)
	}
	return m, nil
}

func (m *model) commitEdit() {
	raw := strings.TrimSpace(m.edit.value)
	field := m.edit.field
	kind := m.edit.kind
	m.edit = editState{}

	switch kind {
	case editURL:
		if raw == "" {
			m.notice = "Server URL cannot be empty."
			return
		}
		m.cfg.BaseURL = raw
		m.notice = "Custom server URL set."
	case editDuration:
		d, err := time.ParseDuration(raw)
		if err != nil || d < 0 {
			m.notice = "Use Go duration syntax, for example 800ms, 4s, or 1m."
			return
		}
		switch field {
		case "warmup":
			m.cfg.Warmup = d
		case "latency":
			if d == 0 {
				m.notice = "Latency duration must be greater than zero."
				return
			}
			m.cfg.LatencyDuration = d
		case "download":
			if d == 0 {
				m.notice = "Download duration must be greater than zero."
				return
			}
			m.cfg.DownloadDuration = d
		case "upload":
			if d == 0 {
				m.notice = "Upload duration must be greater than zero."
				return
			}
			m.cfg.UploadDuration = d
		case "bidirectional":
			if d == 0 {
				m.notice = "Bidirectional duration must be greater than zero."
				return
			}
			m.cfg.BidirectionalDuration = d
		case "ping":
			if d == 0 {
				m.notice = "Ping interval must be greater than zero."
				return
			}
			m.cfg.PingInterval = d
		}
		m.notice = "Timing updated."
	case editInt:
		n, err := strconv.Atoi(raw)
		if err != nil || n < 1 || n > 128 {
			m.notice = "Streams must be an integer from 1 to 128."
			return
		}
		m.cfg.ParallelStreams = n
		m.notice = "Parallel stream count updated."
	}
}

func (m model) activate() (tea.Model, tea.Cmd) {
	switch m.section {
	case sectionServers:
		if m.row < len(serverPresets) {
			preset := serverPresets[m.row]
			m.cfg.BaseURL = preset.url
			m.notice = "Selected " + preset.name + "."
			return m, nil
		}
		m.edit = editState{kind: editURL, field: "url", value: m.cfg.BaseURL}
		m.notice = "Editing server URL. Enter applies, esc cancels."
	case sectionStages:
		switch m.row {
		case 0:
			m.cfg.Stages.Latency = !m.cfg.Stages.Latency
		case 1:
			m.cfg.Stages.Download = !m.cfg.Stages.Download
		case 2:
			m.cfg.Stages.Upload = !m.cfg.Stages.Upload
		case 3:
			m.cfg.Stages.Bidirectional = !m.cfg.Stages.Bidirectional
		case 4:
			m.cfg.LoadedLatency = !m.cfg.LoadedLatency
		}
		m.notice = "Stage profile updated."
	case sectionTiming:
		m.edit = editState{kind: editDuration, value: m.durationValue(m.row)}
		switch m.row {
		case 0:
			m.edit.field = "warmup"
		case 1:
			m.edit.field = "latency"
		case 2:
			m.edit.field = "download"
		case 3:
			m.edit.field = "upload"
		case 4:
			m.edit.field = "bidirectional"
		case 5:
			m.edit.field = "ping"
		}
		m.notice = "Editing duration. Use values like 800ms, 4s, or 1m."
	case sectionNetwork:
		switch m.row {
		case 0:
			m.edit = editState{kind: editInt, field: "streams", value: fmt.Sprintf("%d", m.cfg.ParallelStreams)}
			m.notice = "Editing parallel streams. Use 1 through 128."
		case 1:
			m.cfg.InsecureSkipTLSVerify = !m.cfg.InsecureSkipTLSVerify
			m.notice = "TLS verification setting updated."
		case 2:
			m.cfg = goclient.DefaultConfig()
			m.notice = "Configuration reset to defaults."
		}
	case sectionRun:
		return m.startRun()
	}
	return m, nil
}

func (m model) startRun() (model, tea.Cmd) {
	if !m.cfg.Stages.Latency && !m.cfg.Stages.Download && !m.cfg.Stages.Upload && !m.cfg.Stages.Bidirectional {
		m.notice = "Enable at least one stage before running."
		m.mode = modeConfigure
		m.section = sectionStages
		m.row = 0
		return m, nil
	}

	events := make(chan goclient.Event, 256)
	done := make(chan error, 1)
	ctx, cancel := context.WithCancel(context.Background())
	cfg := m.cfg
	go func() {
		done <- goclient.Run(ctx, cfg, func(e goclient.Event) {
			select {
			case events <- e:
			case <-ctx.Done():
			}
		})
		close(events)
	}()

	m.mode = modeRun
	m.events = events
	m.done = done
	m.cancel = cancel
	m.stage = ""
	m.status = "connecting"
	m.server = ""
	m.err = nil
	m.complete = false
	m.rates = map[goclient.Direction]goclient.ThroughputSample{}
	m.peaks = map[goclient.Direction]float64{}
	m.results = nil
	m.latency = goclient.LatencySample{}
	m.notice = "Run started. Press c or esc to cancel."
	return m, tea.Batch(waitEvent(events), waitDone(done))
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

func (m model) rowCount() int {
	switch m.section {
	case sectionServers:
		return len(serverPresets) + 1
	case sectionStages:
		return 5
	case sectionTiming:
		return 6
	case sectionNetwork:
		return 3
	case sectionRun:
		return 1
	default:
		return 1
	}
}

func (m model) durationValue(row int) string {
	switch row {
	case 0:
		return m.cfg.Warmup.String()
	case 1:
		return m.cfg.LatencyDuration.String()
	case 2:
		return m.cfg.DownloadDuration.String()
	case 3:
		return m.cfg.UploadDuration.String()
	case 4:
		return m.cfg.BidirectionalDuration.String()
	case 5:
		return m.cfg.PingInterval.String()
	default:
		return ""
	}
}

func clamp(v, min, max int) int {
	if max < min {
		return min
	}
	if v < min {
		return min
	}
	if v > max {
		return max
	}
	return v
}

var (
	shellStyle      = lipgloss.NewStyle().Margin(1, 2)
	titleStyle      = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("230")).Background(lipgloss.Color("57")).Padding(0, 1)
	pillStyle       = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("16")).Background(lipgloss.Color("86")).Padding(0, 1)
	panelStyle      = lipgloss.NewStyle().Border(lipgloss.RoundedBorder()).BorderForeground(lipgloss.Color("240")).Padding(1, 2)
	activeTabStyle  = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("230")).Background(lipgloss.Color("63")).Padding(0, 1)
	tabStyle        = lipgloss.NewStyle().Foreground(lipgloss.Color("245")).Padding(0, 1)
	selectedStyle   = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("230")).Background(lipgloss.Color("238"))
	labelStyle      = lipgloss.NewStyle().Foreground(lipgloss.Color("245"))
	valueStyle      = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("86"))
	mutedStyle      = lipgloss.NewStyle().Foreground(lipgloss.Color("244"))
	errorStyle      = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("203"))
	accentStyle     = lipgloss.NewStyle().Foreground(lipgloss.Color("111"))
	warnStyle       = lipgloss.NewStyle().Foreground(lipgloss.Color("215"))
	successStyle    = lipgloss.NewStyle().Foreground(lipgloss.Color("120"))
	subtleRuleStyle = lipgloss.NewStyle().Foreground(lipgloss.Color("239"))
)

func (m model) View() string {
	w := m.width
	if w < 76 {
		w = 76
	}
	inner := w - 4
	if inner > 118 {
		inner = 118
	}

	var b strings.Builder
	b.WriteString(m.header(inner))
	b.WriteString("\n")
	if m.mode == modeRun {
		b.WriteString(m.runView(inner))
	} else {
		b.WriteString(m.configView(inner))
	}
	b.WriteString("\n")
	b.WriteString(m.helpView())
	return shellStyle.Render(b.String())
}

func (m model) header(w int) string {
	status := "ready"
	if m.mode == modeRun {
		status = emptyDash(m.status)
	}
	left := titleStyle.Render("Graphite Meter")
	right := pillStyle.Render(status)
	spacer := strings.Repeat(" ", max(1, w-lipgloss.Width(left)-lipgloss.Width(right)))
	line := left + spacer + right

	target := m.cfg.BaseURL
	if m.server != "" {
		target = m.server
	}
	return line + "\n" + mutedStyle.Render("native go client") + "  " + accentStyle.Render(target)
}

func (m model) configView(w int) string {
	var b strings.Builder
	b.WriteString(m.tabBar(w))
	b.WriteString("\n\n")

	leftW := w
	rightW := w
	if w >= 96 {
		leftW = (w - 2) / 2
		rightW = w - leftW - 2
	}

	menu := panelStyle.Width(leftW).Render(m.sectionView(leftW - 4))
	summary := panelStyle.Width(rightW).Render(m.planView(rightW - 4))
	if w >= 96 {
		b.WriteString(lipgloss.JoinHorizontal(lipgloss.Top, menu, "  ", summary))
	} else {
		b.WriteString(menu)
		b.WriteString("\n\n")
		b.WriteString(summary)
	}
	if m.notice != "" {
		b.WriteString("\n\n")
		b.WriteString(mutedStyle.Render(m.notice))
	}
	return b.String()
}

func (m model) tabBar(w int) string {
	parts := make([]string, 0, len(sectionLabels))
	for i, label := range sectionLabels {
		style := tabStyle
		if section(i) == m.section {
			style = activeTabStyle
		}
		parts = append(parts, style.Render(label))
	}
	line := lipgloss.JoinHorizontal(lipgloss.Left, parts...)
	if lipgloss.Width(line) < w {
		line += subtleRuleStyle.Render(strings.Repeat("─", w-lipgloss.Width(line)))
	}
	return line
}

func (m model) sectionView(w int) string {
	switch m.section {
	case sectionServers:
		return m.serversView(w)
	case sectionStages:
		return m.stagesView(w)
	case sectionTiming:
		return m.timingView(w)
	case sectionNetwork:
		return m.networkView(w)
	case sectionRun:
		return m.runMenuView(w)
	default:
		return ""
	}
}

func (m model) serversView(w int) string {
	lines := []string{accentStyle.Render("Server Selection")}
	active := activePreset(m.cfg.BaseURL)
	for i, preset := range serverPresets {
		mark := " "
		if i == active {
			mark = "●"
		}
		line := fmt.Sprintf("%s %-12s %s", mark, preset.name, mutedStyle.Render(preset.url))
		lines = append(lines, m.menuLine(i, line, w))
		lines = append(lines, mutedStyle.Render("  "+preset.note))
	}
	custom := "○ Custom URL  " + m.cfg.BaseURL
	if active == -1 {
		custom = "● Custom URL  " + m.cfg.BaseURL
	}
	if m.edit.kind == editURL {
		custom = "● Custom URL  " + valueStyle.Render(m.edit.value+"█")
	}
	lines = append(lines, m.menuLine(len(serverPresets), custom, w))
	return lipgloss.JoinVertical(lipgloss.Left, lines...)
}

func (m model) stagesView(w int) string {
	rows := []string{
		toggleLine("Latency", m.cfg.Stages.Latency, "idle RTT baseline"),
		toggleLine("Download", m.cfg.Stages.Download, "server to client"),
		toggleLine("Upload", m.cfg.Stages.Upload, "client to server"),
		toggleLine("Bidirectional", m.cfg.Stages.Bidirectional, "download and upload together"),
		toggleLine("Loaded latency", m.cfg.LoadedLatency, "ping during transfer stages"),
	}
	return m.listWithTitle("Stage Profile", rows, w)
}

func (m model) timingView(w int) string {
	rows := []string{
		valueLine("Warmup", m.cfg.Warmup.String(), "per stage"),
		valueLine("Latency duration", m.cfg.LatencyDuration.String(), "baseline"),
		valueLine("Download duration", m.cfg.DownloadDuration.String(), "transfer"),
		valueLine("Upload duration", m.cfg.UploadDuration.String(), "transfer"),
		valueLine("Bidirectional duration", m.cfg.BidirectionalDuration.String(), "transfer"),
		valueLine("Ping interval", m.cfg.PingInterval.String(), "cadence"),
	}
	if m.edit.kind == editDuration {
		rows[m.row] = valueLine(timingLabel(m.row), m.edit.value+"█", "editing")
	}
	return m.listWithTitle("Timing", rows, w)
}

func (m model) networkView(w int) string {
	streamValue := fmt.Sprintf("%d", m.cfg.ParallelStreams)
	streamNote := "parallel transfers"
	if m.edit.kind == editInt {
		streamValue = m.edit.value + "█"
		streamNote = "editing"
	}
	rows := []string{
		valueLine("Streams", streamValue, streamNote),
		toggleLine("Skip TLS verification", m.cfg.InsecureSkipTLSVerify, "for local/self-signed certs"),
		warnStyle.Render("Reset to defaults"),
	}
	return m.listWithTitle("Network", rows, w)
}

func (m model) runMenuView(w int) string {
	rows := []string{successStyle.Render("Start measurement")}
	return m.listWithTitle("Ready", rows, w)
}

func (m model) listWithTitle(title string, rows []string, w int) string {
	lines := []string{accentStyle.Render(title)}
	for i, row := range rows {
		lines = append(lines, m.menuLine(i, row, w))
	}
	return lipgloss.JoinVertical(lipgloss.Left, lines...)
}

func (m model) menuLine(i int, s string, w int) string {
	prefix := "  "
	if i == m.row {
		prefix = "› "
		s = selectedStyle.Width(max(12, w-2)).Render(s)
	}
	return prefix + s
}

func (m model) planView(w int) string {
	lines := []string{
		accentStyle.Render("Current Plan"),
		labelStyle.Render("Server   ") + valueStyle.Render(m.cfg.BaseURL),
		labelStyle.Render("Stages   ") + valueStyle.Render(stageSummary(m.cfg.Stages)),
		labelStyle.Render("Streams  ") + valueStyle.Render(fmt.Sprintf("%d", m.cfg.ParallelStreams)),
		labelStyle.Render("Warmup   ") + valueStyle.Render(m.cfg.Warmup.String()),
		labelStyle.Render("Ping     ") + valueStyle.Render(m.cfg.PingInterval.String()),
		labelStyle.Render("Loaded   ") + valueStyle.Render(boolLabel(m.cfg.LoadedLatency)),
		labelStyle.Render("TLS      ") + valueStyle.Render(tlsLabel(m.cfg.InsecureSkipTLSVerify)),
		"",
		mutedStyle.Render("Run order"),
	}
	for _, line := range runOrder(m.cfg) {
		lines = append(lines, "  "+line)
	}
	return lipgloss.JoinVertical(lipgloss.Left, lines...)
}

func (m model) runView(w int) string {
	leftW := w
	rightW := w
	if w >= 96 {
		leftW = (w - 2) / 2
		rightW = w - leftW - 2
	}

	summary := panelStyle.Width(leftW).Render(m.summaryView(leftW - 4))
	live := panelStyle.Width(rightW).Render(m.liveView(rightW - 4))
	var b strings.Builder
	if w >= 96 {
		b.WriteString(lipgloss.JoinHorizontal(lipgloss.Top, summary, "  ", live))
	} else {
		b.WriteString(summary)
		b.WriteString("\n\n")
		b.WriteString(live)
	}
	if len(m.results) > 0 {
		b.WriteString("\n\n")
		b.WriteString(panelStyle.Width(w).Render(m.resultsView(w - 4)))
	}
	if m.err != nil {
		b.WriteString("\n\n")
		b.WriteString(errorStyle.Render(m.err.Error()))
	}
	return b.String()
}

func (m model) summaryView(w int) string {
	server := m.server
	if server == "" {
		server = "probing " + m.cfg.BaseURL
	}
	lines := []string{
		accentStyle.Render("Session"),
		labelStyle.Render("Target  ") + valueStyle.Render(server),
		labelStyle.Render("Stage   ") + valueStyle.Render(emptyDash(m.stage)) + mutedStyle.Render(" / "+emptyDash(m.status)),
		labelStyle.Render("Profile ") + valueStyle.Render(stageSummary(m.cfg.Stages)),
		labelStyle.Render("Streams ") + valueStyle.Render(fmt.Sprintf("%d", m.cfg.ParallelStreams)) + mutedStyle.Render("  warmup "+m.cfg.Warmup.String()+"  ping "+m.cfg.PingInterval.String()),
	}
	if m.complete {
		lines = append(lines, "", successStyle.Render("Finished. Press m for menus or r to run again."))
	}
	return lipgloss.JoinVertical(lipgloss.Left, lines...)
}

func (m model) liveView(w int) string {
	down := m.rates[goclient.Down]
	up := m.rates[goclient.Up]
	lines := []string{
		accentStyle.Render("Live Telemetry"),
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

func (m model) helpView() string {
	if m.edit.kind != editNone {
		return mutedStyle.Render("type to edit • enter apply • esc cancel • ctrl+c quit")
	}
	if m.mode == modeRun {
		return mutedStyle.Render("c/esc cancel • m menus after finish • r rerun after finish • q quit")
	}
	return mutedStyle.Render("tab switch menu • ↑/↓ select • enter edit/toggle/select • r run • q quit")
}

func activePreset(url string) int {
	for i, preset := range serverPresets {
		if preset.url == url {
			return i
		}
	}
	return -1
}

func stageSummary(s goclient.StageSet) string {
	var parts []string
	if s.Latency {
		parts = append(parts, "latency")
	}
	if s.Download {
		parts = append(parts, "download")
	}
	if s.Upload {
		parts = append(parts, "upload")
	}
	if s.Bidirectional {
		parts = append(parts, "bidirectional")
	}
	if len(parts) == 0 {
		return "none"
	}
	return strings.Join(parts, ", ")
}

func runOrder(cfg goclient.Config) []string {
	var lines []string
	if cfg.Stages.Latency {
		lines = append(lines, "Latency baseline for "+cfg.LatencyDuration.String())
	}
	if cfg.Stages.Download {
		lines = append(lines, "Download for "+cfg.DownloadDuration.String())
	}
	if cfg.Stages.Upload {
		lines = append(lines, "Upload for "+cfg.UploadDuration.String())
	}
	if cfg.Stages.Bidirectional {
		lines = append(lines, "Bidirectional for "+cfg.BidirectionalDuration.String())
	}
	if len(lines) == 0 {
		return []string{warnStyle.Render("No stages selected")}
	}
	return lines
}

func toggleLine(label string, on bool, note string) string {
	return fmt.Sprintf("%s %-22s %s", checkbox(on), label, mutedStyle.Render(note))
}

func valueLine(label, value, note string) string {
	return fmt.Sprintf("%-24s %s  %s", label, valueStyle.Render(value), mutedStyle.Render(note))
}

func checkbox(on bool) string {
	if on {
		return successStyle.Render("●")
	}
	return mutedStyle.Render("○")
}

func timingLabel(row int) string {
	switch row {
	case 0:
		return "Warmup"
	case 1:
		return "Latency duration"
	case 2:
		return "Download duration"
	case 3:
		return "Upload duration"
	case 4:
		return "Bidirectional duration"
	case 5:
		return "Ping interval"
	default:
		return ""
	}
}

func boolLabel(v bool) string {
	if v {
		return "enabled"
	}
	return "disabled"
}

func tlsLabel(insecure bool) string {
	if insecure {
		return "verification skipped"
	}
	return "verified"
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
	bar := strings.Repeat("█", fill) + mutedStyle.Render(strings.Repeat("░", barW-fill))
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

func max(a, b int) int {
	if a > b {
		return a
	}
	return b
}
