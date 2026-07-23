package main

import (
	"context"
	"errors"
	"fmt"
	"strconv"
	"strings"
	"time"

	tea "github.com/charmbracelet/bubbletea"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
	"github.com/zR-JB/graphite-meter/go/internal/origin"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

type eventsMsg []goclient.Event
type doneMsg struct{ err error }
type preparationMsg struct {
	seq        int
	connection *goclient.PreparedConnection
	err        error
}

// Both auth messages carry the prepareSeq of the preparation that started the
// authorization. A browser approval can stay outstanding for two minutes, so
// without the sequence a poll detached by a server switch can still land and
// overwrite the newer preparation's state.
type authChallengeMsg struct {
	seq     int
	pending *goclient.PendingAuthorization
	err     error
}
type authTokenMsg struct {
	seq    int
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

func beginAuthorization(seq int, cfg goclient.Config, authURL string) tea.Cmd {
	return func() tea.Msg {
		p, err := goclient.BeginAuthorization(cfg, authURL)
		return authChallengeMsg{seq: seq, pending: p, err: err}
	}
}
func pollAuthorization(seq int, p *goclient.PendingAuthorization) tea.Cmd {
	return func() tea.Msg {
		ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
		defer cancel()
		token, err := p.Poll(ctx)
		return authTokenMsg{seq: seq, token: token, origin: p.Origin, err: err}
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
				return m, beginAuthorization(m.prepareSeq, m.cfg, authErr.URL)
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
		if msg.seq != m.prepareSeq {
			return m, nil
		}
		if msg.err != nil {
			m.prepareStatus = "failed"
			m.prepareError = msg.err.Error()
			return m, nil
		}
		m.notice = fmt.Sprintf("Approve in browser: %s  Verification code: %s", msg.pending.BrowserURL, msg.pending.Code)
		return m, pollAuthorization(msg.seq, msg.pending)
	case authTokenMsg:
		if msg.seq != m.prepareSeq {
			return m, nil
		}
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
			return m, beginAuthorization(m.prepareSeq, m.cfg, authErr.URL)
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
			choices := originChoices(m.capabilities().ThroughputTargets, func(t wire.ThroughputTarget) string { return t.Origin })
			m.cfg.ThroughputTarget = nextChoice(m.cfg.ThroughputTarget, choices)
			m.notice = "Throughput endpoint updated."
		case 1:
			choices := originChoices(m.capabilities().LatencyTargets, func(t wire.LatencyTarget) string { return t.Origin })
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
	m.summary = m.summaryView()
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

func activePreset(url string) int {
	for i, preset := range serverPresets {
		if preset.url == url {
			return i
		}
	}
	return -1
}

func (m model) capabilities() wire.Capabilities {
	if m.discovery == nil {
		return wire.Capabilities{}
	}
	return m.discovery.Capabilities
}

// originChoices is the cycle offered for an endpoint row: "auto" first, then each
// discovered origin. Discovery can advertise one origin under several spellings,
// so origin.Key decides equivalence and the first spelling wins.
func originChoices[T any](targets []T, originOf func(T) string) []string {
	choices := []string{"auto"}
	seen := map[string]struct{}{origin.Key("auto"): {}}
	for _, target := range targets {
		value := originOf(target)
		key := origin.Key(value)
		if _, ok := seen[key]; ok {
			continue
		}
		seen[key] = struct{}{}
		choices = append(choices, value)
	}
	return choices
}

func nextChoice(current string, choices []string) string {
	for i, choice := range choices {
		if choice == current {
			return choices[(i+1)%len(choices)]
		}
	}
	return choices[0]
}
