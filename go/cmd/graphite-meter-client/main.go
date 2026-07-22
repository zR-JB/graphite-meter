// Command graphite-meter-client is a native Bubble Tea speedtest client for the
// Graphite Meter server.
package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"os"
	"strconv"
	"strings"
	"time"

	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
	"github.com/zR-JB/graphite-meter/go/internal/origin"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func main() {
	cfg := goclient.DefaultConfig()
	var stages string
	var ping string
	var showVersion bool
	flag.StringVar(&cfg.BaseURL, "url", cfg.BaseURL, "server base URL")
	flag.StringVar(&cfg.ThroughputTarget, "throughput-origin", cfg.ThroughputTarget, "throughput origin from discovery, or auto")
	flag.StringVar(&cfg.ThroughputProtocol, "throughput-protocol", cfg.ThroughputProtocol, "protocol for a negotiated throughput origin: auto, http1, http2, or http3")
	flag.StringVar(&cfg.LatencyTarget, "latency-origin", cfg.LatencyTarget, "WebSocket latency origin from discovery, or auto")
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

	p := tea.NewProgram(newModel(cfg), tea.WithFPS(20))
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

type eventsMsg []goclient.Event
type doneMsg struct{ err error }
type preparationMsg struct {
	seq        int
	connection *goclient.PreparedConnection
	err        error
}
type authChallengeMsg struct {
	pending *goclient.PendingAuthorization
	err     error
}
type authTokenMsg struct {
	token  string
	origin string
	err    error
}

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

var sectionLabels = []string{"Server", "Run setup", "Timing", "Connections", "Start"}

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
	{name: "Local dev", url: "http://127.0.0.1:7246", note: "default HTTP listener"},
	{name: "Local TLS", url: "https://127.0.0.1:7247", note: "dedicated HTTPS HTTP/1.1 listener"},
	{name: "LAN host", url: "http://graphite-meter.local:7246", note: "mDNS or local DNS"},
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

	stage                               string
	status                              string
	server                              string
	target, latencyTarget               string
	throughputProtocol, latencyProtocol string
	err                                 error
	complete                            bool
	prepared                            *goclient.PreparedConnection
	discovery                           *wire.Preflight
	prepareSeq                          int
	prepareStatus                       string
	prepareError                        string

	rates   map[goclient.Direction]goclient.ThroughputSample
	peaks   map[goclient.Direction]float64
	results []goclient.Result
	latency goclient.LatencySample
	summary string
}

func newModel(cfg goclient.Config) model {
	return model{
		cfg:           cfg,
		mode:          modeConfigure,
		notice:        "Choose a server while the selected paths are checked.",
		prepareSeq:    1,
		prepareStatus: "checking",
		rates:         map[goclient.Direction]goclient.ThroughputSample{},
		peaks:         map[goclient.Direction]float64{},
	}
}

func (m model) Init() tea.Cmd {
	return prepareConnection(m.prepareSeq, m.cfg)
}

func prepareConnection(seq int, cfg goclient.Config) tea.Cmd {
	return func() tea.Msg {
		ctx, cancel := context.WithTimeout(context.Background(), 12*time.Second)
		defer cancel()
		connection, err := goclient.Prepare(ctx, cfg)
		return preparationMsg{seq: seq, connection: connection, err: err}
	}
}

func beginAuthorization(cfg goclient.Config, authURL string) tea.Cmd {
	return func() tea.Msg {
		p, err := goclient.BeginAuthorization(cfg, authURL)
		return authChallengeMsg{pending: p, err: err}
	}
}
func pollAuthorization(p *goclient.PendingAuthorization) tea.Cmd {
	return func() tea.Msg {
		ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
		defer cancel()
		token, err := p.Poll(ctx)
		return authTokenMsg{token: token, origin: p.Origin, err: err}
	}
}

func (m model) reprepare(cmd tea.Cmd) (tea.Model, tea.Cmd) {
	m.prepareSeq++
	m.prepareStatus = "checking"
	m.prepareError = ""
	m.prepared = nil
	return m, tea.Batch(cmd, prepareConnection(m.prepareSeq, m.cfg))
}

func waitEvents(events <-chan goclient.Event) tea.Cmd {
	return func() tea.Msg {
		e, ok := <-events
		if !ok {
			return nil
		}
		batch := []goclient.Event{e}
		for {
			select {
			case e, ok := <-events:
				if !ok {
					return eventsMsg(batch)
				}
				batch = append(batch, e)
			default:
				return eventsMsg(batch)
			}
		}
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
	case preparationMsg:
		if msg.seq != m.prepareSeq {
			return m, nil
		}
		if msg.err != nil {
			var authErr *goclient.AuthRequiredError
			if errors.As(msg.err, &authErr) {
				m.prepareStatus = "authorizing"
				m.prepareError = ""
				m.notice = "Authentication is required. Preparing browser approval…"
				return m, beginAuthorization(m.cfg, authErr.URL)
			}
			m.prepareStatus = "failed"
			m.prepareError = msg.err.Error()
			m.prepared = nil
			var preparationErr *goclient.PreparationError
			if errors.As(msg.err, &preparationErr) {
				pf := preparationErr.Preflight
				m.discovery = &pf
			} else {
				m.discovery = nil
			}
		} else {
			m.prepareStatus = "ready"
			m.prepareError = ""
			m.prepared = msg.connection
			pf := msg.connection.Preflight
			m.discovery = &pf
		}
		return m, nil
	case authChallengeMsg:
		if msg.err != nil {
			m.prepareStatus = "failed"
			m.prepareError = msg.err.Error()
			return m, nil
		}
		m.notice = fmt.Sprintf("Approve in browser: %s  Verification code: %s", msg.pending.BrowserURL, msg.pending.Code)
		return m, pollAuthorization(msg.pending)
	case authTokenMsg:
		if msg.err != nil {
			m.prepareStatus = "failed"
			m.prepareError = msg.err.Error()
			return m, nil
		}
		currentOrigin, err := goclient.CanonicalServerOrigin(m.cfg.BaseURL)
		if err != nil || !strings.EqualFold(currentOrigin, msg.origin) {
			m.notice = "Server changed while approval was pending. Authorization was discarded."
			return m.reprepare(nil)
		}
		m.cfg.AuthToken = msg.token
		m.cfg.AuthOrigin = msg.origin
		m.notice = "Client approved. Verifying authenticated transports…"
		return m.reprepare(nil)
	case eventsMsg:
		for _, event := range msg {
			m.apply(event)
		}
		if m.mode == modeRun && m.err == nil {
			return m, waitEvents(m.events)
		}
	case doneMsg:
		var authErr *goclient.AuthRequiredError
		if errors.As(msg.err, &authErr) {
			if m.cancel != nil {
				m.cancel()
			}
			m.complete = true
			m.mode = modeConfigure
			m.prepared = nil
			m.prepareStatus = "authorizing"
			m.notice = "Authentication expired. Preparing browser approval…"
			return m, beginAuthorization(m.cfg, authErr.URL)
		}
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
		before := m.cfg
		updated, cmd := m.activate()
		next := updated.(model)
		if next.mode == modeConfigure && next.edit.kind == editNone && next.cfg != before {
			return next.reprepare(cmd)
		}
		return next, cmd
	case "r":
		return m.startRun()
	case "v":
		return m.reprepare(nil)
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
		before := m.cfg
		m.commitEdit()
		if m.cfg != before {
			return m.reprepare(nil)
		}
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
		if raw != m.cfg.BaseURL {
			m.cfg.AuthToken = ""
			m.cfg.AuthOrigin = ""
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
		min := 0
		if field == "auto-streams" {
			min = 1
		}
		if err != nil || n < min || n > 128 {
			if field == "auto-streams" {
				m.notice = "Automatic H1 max must be an integer from 1 to 128."
				return
			}
			m.notice = "Streams must be 0 (automatic) or an integer from 1 to 128."
			return
		}
		if field == "auto-streams" {
			m.cfg.TransferStreams.AutomaticMax = n
		} else {
			m.cfg.TransferStreams.Forced = n
		}
		m.notice = "Transfer stream policy updated."
	}
}

func (m model) activate() (tea.Model, tea.Cmd) {
	switch m.section {
	case sectionServers:
		if m.row < len(serverPresets) {
			preset := serverPresets[m.row]
			if preset.url != m.cfg.BaseURL {
				m.cfg.AuthToken = ""
				m.cfg.AuthOrigin = ""
			}
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
			choices := throughputOriginChoices(m.discovery)
			m.cfg.ThroughputTarget = nextChoice(m.cfg.ThroughputTarget, choices)
			m.notice = "Throughput endpoint updated."
		case 1:
			choices := latencyOriginChoices(m.discovery)
			m.cfg.LatencyTarget = nextChoice(m.cfg.LatencyTarget, choices)
			m.notice = "Latency endpoint updated."
		case 2:
			m.cfg.ThroughputProtocol = nextChoice(m.cfg.ThroughputProtocol, []string{"auto", "http1", "http2", "http3"})
			m.notice = "Throughput protocol updated."
		case 3:
			m.edit = editState{kind: editInt, field: "auto-streams", value: fmt.Sprintf("%d", m.cfg.TransferStreams.AutomaticMax)}
			m.notice = "Editing maximum automatic HTTP/1 streams. Use 1 through 128."
		case 4:
			m.edit = editState{kind: editInt, field: "streams", value: fmt.Sprintf("%d", m.cfg.TransferStreams.Forced)}
			m.notice = "Editing streams per direction. Use 0 for automatic or 1 through 128 to force."
		case 5:
			m.cfg.InsecureSkipTLSVerify = !m.cfg.InsecureSkipTLSVerify
			m.notice = "TLS verification setting updated."
		case 6:
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
	prepared := m.prepared
	go func() {
		run := goclient.Run
		if prepared.FreshFor(cfg) {
			run = func(ctx context.Context, cfg goclient.Config, emit func(goclient.Event)) error {
				return goclient.RunPrepared(ctx, cfg, prepared, emit)
			}
		}
		runErr := run(ctx, cfg, func(e goclient.Event) {
			select {
			case events <- e:
			case <-ctx.Done():
			}
		})
		done <- goclient.ClassifyAuthFailure(ctx, cfg, runErr)
		close(events)
	}()

	m.mode = modeRun
	m.events = events
	m.done = done
	m.cancel = cancel
	m.stage = ""
	m.status = "connecting"
	m.server = ""
	m.target, m.latencyTarget = "", ""
	m.throughputProtocol, m.latencyProtocol = "", ""
	m.err = nil
	m.complete = false
	m.rates = map[goclient.Direction]goclient.ThroughputSample{}
	m.peaks = map[goclient.Direction]float64{}
	m.results = nil
	m.latency = goclient.LatencySample{}
	m.notice = "Run started. Press c or esc to cancel."
	m.refreshSummary()
	return m, tea.Batch(waitEvents(events), waitDone(done))
}

func (m *model) apply(e goclient.Event) {
	switch e.Kind {
	case goclient.EventPreflight:
		if e.Preflight != nil {
			m.target = e.ThroughputTarget
			if m.target == "" {
				m.target = e.Message
			}
			m.latencyTarget = e.LatencyTarget
			m.throughputProtocol = e.ThroughputProtocol
			m.latencyProtocol = e.LatencyProtocol
			observed := ""
			if e.Probe != nil {
				observed = "/" + e.Probe.ProtocolNegotiated
			}
			m.server = fmt.Sprintf("%s %s [%s%s]", e.Preflight.Server.Name, e.Preflight.Server.Location, e.Message, observed)
			m.status = "connected"
		}
		m.refreshSummary()
	case goclient.EventStage:
		m.stage = e.Stage
		m.status = e.Message
		m.refreshSummary()
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
		m.refreshSummary()
	case goclient.EventError:
		m.err = e.Err
		m.status = "error"
		m.refreshSummary()
	}
}

func (m *model) refreshSummary() {
	m.summary = m.summaryView(0)
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
		return 7
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

func throughputOriginChoices(pf *wire.Preflight) []string {
	choices := []string{"auto"}
	if pf == nil {
		return choices
	}
	seen := map[string]struct{}{origin.Key("auto"): {}}
	for _, target := range pf.Capabilities.ThroughputTargets {
		choices = appendUniqueOriginChoice(choices, seen, target.Origin)
	}
	return choices
}

func latencyOriginChoices(pf *wire.Preflight) []string {
	choices := []string{"auto"}
	if pf == nil {
		return choices
	}
	seen := map[string]struct{}{origin.Key("auto"): {}}
	for _, target := range pf.Capabilities.LatencyTargets {
		choices = appendUniqueOriginChoice(choices, seen, target.Origin)
	}
	return choices
}

func appendUniqueOriginChoice(choices []string, seen map[string]struct{}, value string) []string {
	key := origin.Key(value)
	if _, ok := seen[key]; ok {
		return choices
	}
	seen[key] = struct{}{}
	return append(choices, value)
}

func nextChoice(current string, choices []string) string {
	for i, choice := range choices {
		if choice == current {
			return choices[(i+1)%len(choices)]
		}
	}
	return choices[0]
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
	return line + "\n" + mutedStyle.Render("native go client "+goclient.Version) + "  " + accentStyle.Render(target)
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
	throughput := targetChoiceLabel(m.cfg.ThroughputTarget)
	latency := targetChoiceLabel(m.cfg.LatencyTarget)
	if m.prepared.FreshFor(m.cfg) {
		throughput = m.prepared.ThroughputSummary()
		latency = m.prepared.LatencySummary()
	}
	rows := []string{
		valueLine("Throughput endpoint", throughput, "independent path"),
		valueLine("Latency endpoint", latency, "independent path"),
		valueLine("Throughput protocol", m.cfg.ThroughputProtocol, "negotiated endpoints"),
		valueLine("Auto H1 max", fmt.Sprintf("%d", m.cfg.TransferStreams.AutomaticMax), "per direction"),
		valueLine("Streams", m.cfg.TransferStreams.Label(m.cfg.ThroughputProtocol), "0 automatic; 1–128 forced"),
		toggleLine("Unsafe: skip TLS verification", m.cfg.InsecureSkipTLSVerify, "advanced; all native TLS requests"),
		warnStyle.Render("Reset to defaults"),
	}
	if m.edit.field == "auto-streams" {
		rows[3] = valueLine("Auto H1 max", m.edit.value+"█", "editing")
	}
	if m.edit.field == "streams" {
		rows[4] = valueLine("Streams", m.edit.value+"█", "editing")
	}
	return m.listWithTitle("Connections", rows, w)
}

func (m model) runMenuView(w int) string {
	label := "Start measurement · " + m.prepareStatus
	if m.prepareStatus == "ready" {
		label = successStyle.Render(label)
	} else if m.prepareStatus == "failed" {
		label = warnStyle.Render(label)
	}
	return m.listWithTitle("Connection readiness", []string{label}, w)
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
	throughput := "Checking"
	latency := "Checking"
	observed := ""
	if m.prepared.FreshFor(m.cfg) {
		throughput = m.prepared.ThroughputSummary()
		latency = m.prepared.LatencySummary()
		observed = m.prepared.Probe.ProtocolNegotiated
	}
	lines := []string{
		accentStyle.Render("Resolved Plan"),
		labelStyle.Render("Status     ") + valueStyle.Render(m.prepareStatus),
		labelStyle.Render("Throughput ") + valueStyle.Render(throughput),
		labelStyle.Render("Latency    ") + valueStyle.Render(latency),
		labelStyle.Render("Observed   ") + valueStyle.Render(emptyDash(observed)),
		"",
		mutedStyle.Render("Run order"),
	}
	if m.prepareError != "" {
		lines = append(lines, warnStyle.Render(m.prepareError), mutedStyle.Render("Press v to retry."), "")
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

	summaryBody := m.summary
	if summaryBody == "" {
		summaryBody = m.summaryView(leftW - 4)
	}
	summary := panelStyle.Width(leftW).Render(summaryBody)
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
	mark := ""
	switch {
	case m.complete:
		mark = successStyle.Render("✓ ")
	case m.stage != "":
		mark = accentStyle.Render("• ")
	}
	lines := []string{
		accentStyle.Render("Session"),
		labelStyle.Render("Target  ") + valueStyle.Render(server),
		labelStyle.Render("Stage   ") + mark + valueStyle.Render(emptyDash(m.stage)) + mutedStyle.Render(" / "+emptyDash(m.status)),
		labelStyle.Render("Profile ") + valueStyle.Render(stageSummary(m.cfg.Stages)),
		labelStyle.Render("Throughput") + valueStyle.Render(" "+emptyDash(m.target)+" · "+emptyDash(m.throughputProtocol)),
		labelStyle.Render("Latency   ") + valueStyle.Render(" "+emptyDash(m.latencyTarget)+" · websocket · "+emptyDash(m.latencyProtocol)),
		labelStyle.Render("Progress  ") + valueStyle.Render(" selected throughput path"),
		labelStyle.Render("Streams ") + valueStyle.Render(m.cfg.TransferStreams.Label(m.target)) + mutedStyle.Render("  warmup "+m.cfg.Warmup.String()+"  ping "+m.cfg.PingInterval.String()),
	}
	if m.complete {
		lines = append(lines, "", successStyle.Render("Finished. Press m for menus or r to run again."))
	}
	return lipgloss.JoinVertical(lipgloss.Left, lines...)
}

func (m model) liveView(w int) string {
	// One shared scale (the larger of the two session peaks) for both bars, so a
	// glance at the fill compares download against upload directly instead of each
	// bar being full against its own peak.
	scale := m.rateScale()
	lines := []string{
		accentStyle.Render("Live Telemetry"),
		rateLine("download", m.rates[goclient.Down].BytesPerSec, scale, w),
		rateLine("upload  ", m.rates[goclient.Up].BytesPerSec, scale, w),
		latencyLine(m.latency),
	}
	return lipgloss.JoinVertical(lipgloss.Left, lines...)
}

// rateScale is the shared bar denominator: the larger session peak across both
// directions. Peaks only grow, so the scale is stable within a run.
func (m model) rateScale() float64 {
	s := m.peaks[goclient.Down]
	if p := m.peaks[goclient.Up]; p > s {
		s = p
	}
	return s
}

func (m model) resultsView(w int) string {
	lines := []string{accentStyle.Render("Results")}

	// One shared scale across every throughput row (both directions) so the mini
	// bars are comparable at a glance — the same principle as the live bars.
	var scale float64
	for _, r := range m.results {
		if !isLatencyResult(r) && r.PeakBps > scale {
			scale = r.PeakBps
		}
	}
	barW := clamp(w-56, 10, 30)

	for _, r := range m.results {
		if isLatencyResult(r) {
			lines = append(lines, fmt.Sprintf("%s %s   p50 %s  p95 %s  %s",
				mutedStyle.Render(fmt.Sprintf("%-13s", r.Stage)),
				labelStyle.Render("latency"),
				valueStyle.Render(fmtMs(r.Latency.P50)),
				valueStyle.Render(fmtMs(r.Latency.P95)),
				mutedStyle.Render(fmt.Sprintf("jitter %s  loss %.1f%%", fmtMs(r.Latency.Jitter), r.Latency.Loss*100)),
			))
			continue
		}
		n := 0
		if scale > 0 {
			n = int((r.MeanBps/scale)*float64(barW) + 0.5)
		}
		n = clamp(n, 0, barW)
		bar := accentStyle.Render(strings.Repeat("█", n)) + mutedStyle.Render(strings.Repeat("░", barW-n))
		dir := "down"
		if r.Direction == goclient.Up {
			dir = "up"
		}
		auth := ""
		if r.ServerAuth {
			auth = mutedStyle.Render(" server-clock")
		}
		lines = append(lines, fmt.Sprintf("%s %s %s  %s  %s %s%s",
			mutedStyle.Render(fmt.Sprintf("%-13s", r.Stage)),
			accentStyle.Render(fmt.Sprintf("%-4s", dir)),
			bar,
			valueStyle.Render(fmt.Sprintf("%13s", fmtRate(r.MeanBps))),
			mutedStyle.Render("peak "+fmtRate(r.PeakBps)),
			mutedStyle.Render("total "+fmtBytes(r.TotalBytes)),
			auth,
		))
	}
	return lipgloss.JoinVertical(lipgloss.Left, lines...)
}

// isLatencyResult reports whether a result carries latency percentiles rather
// than throughput figures.
func isLatencyResult(r goclient.Result) bool {
	return r.Latency.Count > 0 || r.Stage == "latency"
}

func (m model) helpView() string {
	if m.edit.kind != editNone {
		return mutedStyle.Render("type to edit • enter apply • esc cancel • ctrl+c quit")
	}
	if m.mode == modeRun {
		return mutedStyle.Render("c/esc cancel • m menus after finish • r rerun after finish • q quit")
	}
	return mutedStyle.Render("tab switch menu • ↑/↓ select • enter edit/toggle/select • v recheck • r run • q quit")
}

func targetChoiceLabel(target string) string {
	labels := map[string]string{
		"auto":           "Automatic",
		"http1-clear":    "HTTP/1.1 · clear",
		"http1-tls":      "HTTP/1.1 · TLS",
		"http2":          "HTTP/2 · TLS",
		"http3":          "HTTP/3 · QUIC",
		"ws-http1-clear": "WebSocket · HTTP/1.1 · clear",
		"ws-http1-tls":   "WebSocket · HTTP/1.1 · TLS",
	}
	if label := labels[target]; label != "" {
		return label
	}
	return target
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

// rateLine renders one telemetry bar filled by rate/scale (a scale shared across
// directions), with a brighter leading cell so motion reads at a glance and the
// eased rate as the trailing figure.
func rateLine(name string, rate, scale float64, w int) string {
	barW := w - 34
	if barW < 12 {
		barW = 12
	}
	n := 0
	if scale > 0 {
		n = int((rate/scale)*float64(barW) + 0.5)
	}
	n = clamp(n, 0, barW)
	var bar string
	if n <= 0 {
		bar = mutedStyle.Render(strings.Repeat("░", barW))
	} else {
		bar = accentStyle.Render(strings.Repeat("█", n-1)) +
			valueStyle.Render("█") +
			mutedStyle.Render(strings.Repeat("░", barW-n))
	}
	return fmt.Sprintf("%s %s %12s", labelStyle.Render(name), bar, valueStyle.Render(fmtRate(rate)))
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
