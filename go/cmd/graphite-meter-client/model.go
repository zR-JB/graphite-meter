package main

import (
	"context"
	"fmt"
	"net/url"
	"reflect"
	"slices"
	"strconv"
	"strings"
	"time"

	"github.com/charmbracelet/bubbles/cursor"
	"github.com/charmbracelet/bubbles/help"
	"github.com/charmbracelet/bubbles/key"
	"github.com/charmbracelet/bubbles/spinner"
	"github.com/charmbracelet/bubbles/textinput"
	tea "github.com/charmbracelet/bubbletea"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
	"github.com/zR-JB/graphite-meter/go/internal/origin"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

type eventsMsg struct {
	seq    int
	events []goclient.Event
}
type preparationMsg struct {
	run        *goclient.PreparedRun
	seq        int
	connection *goclient.PreparedConnection
	err        error
}

type prepareDueMsg struct{ seq int }

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
	sectionRunSetup
	sectionTiming
	sectionConnections
	sectionRun
	sectionCount
)

var sectionLabels = []string{"Server", "Run setup", "Timing", "Connections", "Start"}

const (
	rowThroughputPath = iota
	rowThroughputProtocol
	rowLatencyPath
	rowAutoStreams
	rowStreams
	rowSkipTLS
	rowReset
	rowConnectionsCount
)

const protocolNegotiated = "negotiated"

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
	input textinput.Model
	err   string
}

func beginEdit(kind editKind, field, value string) editState {
	in := textinput.New()
	in.Prompt = ""
	in.TextStyle = valueStyle
	in.Cursor.SetMode(cursor.CursorStatic)
	in.SetValue(value)
	in.Focus()
	return editState{kind: kind, field: field, input: in}
}

type prepareStep int

const (
	stepReach     prepareStep = iota
	stepPreflight             // an auth challenge proves the server answered
	stepOrigins               // a PreparationError proves the preflight body decoded
	stepReady                 // a finished preparation proves the origins resolved
)

type checkState int

const (
	checkPending checkState = iota
	checkActive
	checkDone
	checkFailed
	checkSkipped
)

type check struct {
	state checkState
	label string
	note  string
}

const statusIdle = "not checked"

func (m model) connectionChecks() []check {
	state := func(step prepareStep) checkState {
		switch {
		case m.prepareStatus == statusIdle:
			return checkPending
		case m.prepareStep > step:
			return checkDone
		case m.prepareStep < step:
			return checkPending
		case m.prepareStatus == "failed":
			return checkFailed
		case m.prepareStatus == "authorizing":
			return checkPending
		default:
			return checkActive
		}
	}
	auth := check{label: "authorization"}
	switch {
	case m.prepareStatus == "authorizing":
		auth.state, auth.note = checkActive, "browser approval"
	case m.cfg.AuthToken != "":
		auth.state, auth.note = checkDone, "client approved"
	case m.prepareStep == stepReady:
		auth.state, auth.note = checkSkipped, "not required"
	}
	return []check{
		{state: state(stepReach), label: "endpoint reachable"},
		{state: state(stepPreflight), label: "preflight accepted"},
		auth,
		{state: state(stepOrigins), label: "origins resolved"},
	}
}

type serverPreset struct {
	name string
	url  string
	note string
}

var serverPresets = []serverPreset{
	{name: "Local dev", url: "http://127.0.0.1:7246", note: "HTTP listener"},
}

type model struct {
	serverDetailsOpen  bool
	detailsScroll      int
	serverResults      map[string][]goclient.Result
	preparedRun        *goclient.PreparedRun
	runDetails         *goclient.RunDetails
	serverChooser      bool
	chooseAfterPrepare bool
	serverDraft        []string
	serverRow          int
	height             int
	latencyFocus       string
	latencyByServer    map[string]goclient.LatencySample
	lostByServer       map[string]int
	authServerID       string
	controller         *goclient.Controller

	cfg   goclient.Config
	mode  mode
	width int

	section      section
	row          int
	edit         editState
	notice       string
	spin         spinner.Model
	help         help.Model
	cancelPrompt bool
	now          time.Time

	// runSeq stamps every message a run emits; a superseded run's messages carry an older sequence and are dropped.
	runSeq int
	events <-chan goclient.Event

	displayRates map[goclient.Direction]float64
	lostStreak   int

	stage                                 string
	status                                string
	server                                string
	target, latencyTarget                 string
	throughputProtocol, latencyProtocol   string
	throughputTransport, latencyTransport string

	err           error
	complete      bool
	prepared      *goclient.PreparedConnection
	discovery     *wire.Preflight
	prepareSeq    int
	preparation   *goclient.Preparation
	prepareStatus string
	prepareStep   prepareStep
	prepareError  string
	auth          *goclient.PendingAuthorization
	authSince     time.Time
	authOpened    bool
	// openApproval launches the browser. It is a field so a test can watch the call instead of opening a window.
	openApproval func(*goclient.PendingAuthorization)

	rates   map[goclient.Direction]goclient.ThroughputSample
	peaks   map[goclient.Direction]float64
	results []goclient.Result
	latency goclient.LatencySample
	stages  []stageProgress
}

const animationFPS = 20

func newModel(cfg goclient.Config) model {
	controller := goclient.NewController(context.Background())
	dial := spinner.MiniDot
	dial.FPS = time.Second / animationFPS
	spin := spinner.New(spinner.WithSpinner(dial))
	spin.Style = accentStyle
	return model{
		controller:    controller,
		preparation:   controller.NewPreparation(cfg),
		cfg:           cfg,
		mode:          modeConfigure,
		openApproval:  (*goclient.PendingAuthorization).Open,
		notice:        "Press enter on a server to check it.",
		prepareSeq:    1,
		prepareStatus: statusIdle,
		spin:          spin,
		help:          newHelp(),
		now:           time.Now(),
		rates:         map[goclient.Direction]goclient.ThroughputSample{},
		peaks:         map[goclient.Direction]float64{},
		displayRates:  map[goclient.Direction]float64{},
	}
}

func (m model) Init() tea.Cmd { return nil }

func (m model) animating() bool {
	if m.mode == modeRun {
		return !m.complete
	}
	return m.prepareStatus == "checking" || m.prepareStatus == "authorizing"
}

func (m model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.WindowSizeMsg:
		m.width = msg.Width
		m.height = msg.Height
		return m, nil
	case tea.KeyMsg:
		return m.handleKey(msg)
	case spinner.TickMsg:
		return m.handleTick(msg)
	case prepareDueMsg:
		return m.handlePrepareDue(msg)
	case preparationMsg:
		return m.handlePreparation(msg)
	case authChallengeMsg:
		return m.handleAuthChallenge(msg)
	case authTokenMsg:
		return m.handleAuthToken(msg)
	case eventsMsg:
		return m.handleEvents(msg)
	default:
		// The clipboard read behind ctrl+v answers with a message only the text input understands.
		if m.edit.kind != editNone {
			var cmd tea.Cmd
			m.edit.input, cmd = m.edit.input.Update(msg)
			return m, cmd
		}
	}
	return m, nil
}

func (m model) handleKey(msg tea.KeyMsg) (tea.Model, tea.Cmd) {
	switch {
	case m.serverDetailsOpen:
		return m.handleServerDetailsKey(msg)
	case m.serverChooser:
		return m.handleServerChooserKey(msg)
	case m.edit.kind != editNone:
		return m.handleEditKey(msg)
	case m.auth != nil && !m.authOpened && key.Matches(msg, keys.approve):
		m.openApproval(m.auth)
		m.authOpened = true
		m.notice = "Approval page opened in the browser."
		return m, nil
	case m.mode == modeConfigure && m.canChooseServers() && key.Matches(msg, keys.servers):
		return m.openServerChooser()
	case m.mode == modeConfigure && key.Matches(msg, keys.automatic):
		return m.useAutomatic()
	case m.urlRowRune(msg):
		// A bracketed paste arrives as one rune message and lands whole.
		m.edit = beginEdit(editURL, "url", string(msg.Runes))
		m.notice = "Editing server URL. Enter applies, esc cancels."
		return m, nil
	case key.Matches(msg, keys.quit):
		m.close()
		return m, tea.Quit
	case m.cancelPrompt:
		return m.answerCancelPrompt(msg)
	case key.Matches(msg, keys.help):
		m.help.ShowAll = !m.help.ShowAll
		return m, nil
	case m.mode == modeRun:
		return m.handleRunKey(msg)
	}
	return m.handleConfigureKey(msg)
}

func (m model) handleConfigureKey(msg tea.KeyMsg) (tea.Model, tea.Cmd) {
	step := 1
	if reverse(msg) {
		step = -1
	}
	switch {
	case m.canChooseServers() && key.Matches(msg, keys.servers):
		return m.openServerChooser()
	case key.Matches(msg, keys.automatic):
		return m.useAutomatic()
	case key.Matches(msg, keys.sections):
		m.section = section((int(m.section) + step + int(sectionCount)) % int(sectionCount))
		m.row = clamp(m.row, 0, m.rowCount()-1)
	case key.Matches(msg, keys.rows):
		m.row = clamp(m.row+step, 0, m.rowCount()-1)
	case key.Matches(msg, keys.activate):
		return m.confirm()
	case key.Matches(msg, keys.run):
		return m.startRun()
	case key.Matches(msg, keys.verify):
		return m.reprepare(nil)
	}
	return m, nil
}

func (m model) urlRowRune(msg tea.KeyMsg) bool {
	return m.mode == modeConfigure && !m.cancelPrompt &&
		m.section == sectionServers && m.row == len(serverPresets) &&
		msg.Type == tea.KeyRunes && !msg.Alt
}

func (m model) handleRunKey(msg tea.KeyMsg) (tea.Model, tea.Cmd) {
	if key.Matches(msg, keys.serverDetails) && m.hasServerBreakdown() {
		m.serverDetailsOpen = true
		m.detailsScroll = 0
		return m, nil
	}
	if key.Matches(msg, keys.latencyFocus) {
		m.nextLatencyFocus()
		return m, nil
	}
	if !m.complete {
		if key.Matches(msg, keys.cancel) {
			m.cancelPrompt = true
			m.notice = "Cancel the run? esc confirms, any other key continues."
		}
		return m, nil
	}
	switch {
	case key.Matches(msg, keys.back):
		m.mode = modeConfigure
		m.section = sectionRun
		m.row = 0
	case key.Matches(msg, keys.rerun):
		return m.startRun()
	}
	return m, nil
}

func (m model) answerCancelPrompt(msg tea.KeyMsg) (tea.Model, tea.Cmd) {
	m.cancelPrompt = false
	if !key.Matches(msg, keys.confirm) {
		m.notice = "Run continues."
		return m, nil
	}
	m.controller.CancelRun()
	m.status = "canceling"
	m.notice = "Canceling the run."
	return m, nil
}

func (m *model) focusServer() {
	m.mode = modeConfigure
	if m.edit.kind != editNone {
		return
	}
	m.section = sectionServers
	m.row = len(serverPresets)
	if i := activePreset(m.cfg.BaseURL); i >= 0 {
		m.row = i
	}
}

func (m model) confirm() (tea.Model, tea.Cmd) {
	before := m.cfg
	updated, cmd := m.activate()
	next := updated.(model)
	if next.mode != modeConfigure || next.edit.kind != editNone {
		return next, cmd
	}
	if !reflect.DeepEqual(next.cfg, before) || (m.section == sectionServers && m.row < len(serverPresets)) {
		return next.reprepare(cmd)
	}
	return next, cmd
}

func (m model) handleEditKey(msg tea.KeyMsg) (tea.Model, tea.Cmd) {
	switch {
	case key.Matches(msg, keys.abort):
		m.close()
		return m, tea.Quit
	case key.Matches(msg, keys.discard):
		m.notice = "Edit canceled."
		m.edit = editState{}
		return m, nil
	case key.Matches(msg, keys.apply):
		before, wasURL := m.cfg, m.edit.kind == editURL
		m.commitEdit()
		if !reflect.DeepEqual(m.cfg, before) || (wasURL && m.edit.kind == editNone) {
			return m.reprepare(nil)
		}
		return m, nil
	}
	m.edit.err = ""
	var cmd tea.Cmd
	m.edit.input, cmd = m.edit.input.Update(msg)
	return m, cmd
}

// editRejected keeps the field open so the offending text stays editable.
func (m *model) editRejected(reason string) {
	m.edit.err = reason
	m.notice = reason
}

func (m *model) editAccepted(notice string) {
	m.edit = editState{}
	m.notice = notice
}

func (m *model) commitEdit() {
	raw := strings.TrimSpace(m.edit.input.Value())
	switch m.edit.kind {
	case editURL:
		m.commitURL(raw)
	case editDuration:
		m.commitDuration(raw, m.edit.field)
	case editInt:
		m.commitInt(raw, m.edit.field)
	}
}

func (m *model) commitURL(raw string) {
	if raw == "" {
		m.editRejected("Server URL cannot be empty.")
		return
	}
	if !strings.Contains(raw, "://") {
		raw = "http://" + raw
	}
	if u, err := url.Parse(raw); err != nil || (u.Scheme != "http" && u.Scheme != "https") || u.Host == "" {
		m.editRejected("Use an http:// or https:// URL with a host, for example https://meter.example.")
		return
	}
	if raw != m.cfg.BaseURL {
		m.cfg.AuthToken = ""
		m.cfg.AuthOrigin = ""
	}
	m.cfg.BaseURL = raw
	m.editAccepted("Custom server URL set.")
}

func (m *model) commitDuration(raw, field string) {
	// A bare number is seconds, so "10" works as well as "10s".
	if n, err := strconv.ParseFloat(raw, 64); err == nil {
		raw = fmt.Sprintf("%gs", n)
	}
	d, err := time.ParseDuration(raw)
	if err != nil || d < 0 {
		m.editRejected("Use a duration like 800ms, 4s, or 1m — a bare number is seconds.")
		return
	}
	for _, setting := range timingSettings(&m.cfg) {
		if setting.name != field {
			continue
		}
		if d == 0 && field != "warmup" {
			label := setting.label
			if field != "ping" {
				label += " duration"
			}
			m.editRejected(label + " must be greater than zero.")
			return
		}
		if field == "ping" && goclient.PingIntervalBoundApplies(m.cfg.LatencyTransport) {
			if err := goclient.ValidatePingInterval(d); err != nil {
				m.editRejected(err.Error())
				return
			}
		}
		*setting.value = d
	}
	m.editAccepted("Timing updated.")
}

func (m *model) commitInt(raw, field string) {
	n, err := strconv.Atoi(raw)
	lowest := 0
	if field == "auto-streams" {
		lowest = 1
	}
	if err != nil || n < lowest || n > 128 {
		if field == "auto-streams" {
			m.editRejected("Automatic H1 max must be an integer from 1 to 128.")
			return
		}
		m.editRejected("Streams must be 0 (automatic) or an integer from 1 to 128.")
		return
	}
	if field == "auto-streams" {
		m.cfg.TransferStreams.AutomaticMax = n
	} else {
		m.cfg.TransferStreams.Forced = n
	}
	m.editAccepted("Transfer stream policy updated.")
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
		m.edit = beginEdit(editURL, "url", m.cfg.BaseURL)
		m.notice = "Editing server URL. Enter applies, esc cancels."
	case sectionRunSetup:
		setting := stageSettings(&m.cfg)[m.row]
		*setting.value = !*setting.value
		m.notice = "Stage profile updated."
	case sectionTiming:
		setting := timingSettings(&m.cfg)[m.row]
		m.edit = beginEdit(editDuration, setting.name, setting.value.String())
		m.notice = "Editing duration. Use values like 800ms, 4s, or 1m."
	case sectionConnections:
		switch m.row {
		case rowThroughputPath:
			next := nextPath(m.cfg.ThroughputTarget, m.cfg.ThroughputTransport, m.throughputPaths())
			m.cfg.ThroughputTarget, m.cfg.ThroughputTransport = next.target, next.transport
			if t := m.selectedThroughputPath(); !m.multipleServers() && t != nil && t.Protocol != protocolNegotiated {
				m.cfg.ThroughputProtocol = "auto"
			}
			m.notice = "Throughput path set to " + next.label + "."
		case rowThroughputProtocol:
			if t := m.selectedThroughputPath(); !m.multipleServers() && t != nil && t.Protocol != protocolNegotiated {
				m.notice = "This path serves " + goclient.ConnectionSummary(t.Transport, t.Protocol, t.TLS) + " only."
				break
			}
			m.cfg.ThroughputProtocol = nextChoice(m.cfg.ThroughputProtocol, []string{"auto", "http1", "http2", "http3"})
			m.notice = "Throughput HTTP version updated."
		case rowLatencyPath:
			next := nextPath(m.cfg.LatencyTarget, m.cfg.LatencyTransport, m.latencyPaths())
			m.cfg.LatencyTarget, m.cfg.LatencyTransport = next.target, next.transport
			m.notice = "Latency path set to " + next.label + "."
		case rowAutoStreams:
			m.edit = beginEdit(editInt, "auto-streams", fmt.Sprintf("%d", m.cfg.TransferStreams.AutomaticMax))
			m.notice = "Editing maximum automatic HTTP/1 streams. Use 1 through 128."
			if m.cfg.ThroughputTransport == wire.TransportWebTransport {
				m.notice += " WebTransport ignores it."
			}
		case rowStreams:
			m.edit = beginEdit(editInt, "streams", fmt.Sprintf("%d", m.cfg.TransferStreams.Forced))
			m.notice = "Editing streams per direction. Use 0 for automatic or 1 through 128 to force."
		case rowSkipTLS:
			m.cfg.InsecureSkipTLSVerify = !m.cfg.InsecureSkipTLSVerify
			m.notice = "TLS verification setting updated."
		case rowReset:
			m.cfg = goclient.DefaultConfig()
			m.notice = "Configuration reset to defaults."
		}
	case sectionRun:
		return m.startRun()
	}
	return m, nil
}

func (m model) rowCount() int {
	switch m.section {
	case sectionServers:
		return len(serverPresets) + 1
	case sectionRunSetup:
		return len(stageSettings(&m.cfg))
	case sectionTiming:
		return len(timingSettings(&m.cfg))
	case sectionConnections:
		return rowConnectionsCount
	case sectionRun:
		return 1
	default:
		return 1
	}
}

type setting[T any] struct {
	name, label, note string
	value             *T
}

// Resolve pointers against the current model copy; never retain them across Update calls.
func timingSettings(cfg *goclient.Config) []setting[time.Duration] {
	return []setting[time.Duration]{
		{"warmup", "Warmup", "per stage, before the clock starts", &cfg.Warmup},
		{"latency", "Latency", "measured window", &cfg.LatencyDuration},
		{"download", "Download", "measured window", &cfg.DownloadDuration},
		{"upload", "Upload", "measured window", &cfg.UploadDuration},
		{"bidirectional", "Bidirectional", "measured window", &cfg.BidirectionalDuration},
		{"ping", "Ping interval", "cadence", &cfg.PingInterval},
	}
}

func stageSettings(cfg *goclient.Config) []setting[bool] {
	return []setting[bool]{
		{"latency", "Latency", "idle RTT baseline", &cfg.Stages.Latency},
		{"download", "Download", "server to client", &cfg.Stages.Download},
		{"upload", "Upload", "client to server", &cfg.Stages.Upload},
		{"bidirectional", "Bidirectional", "both directions at once", &cfg.Stages.Bidirectional},
		{"loaded", "Loaded latency", "ping during transfers", &cfg.LoadedLatency},
	}
}

func activePreset(url string) int {
	return slices.IndexFunc(serverPresets, func(preset serverPreset) bool { return preset.url == url })
}

func (m model) capabilities() wire.Capabilities {
	if m.discovery == nil {
		return wire.Capabilities{}
	}
	return m.discovery.Capabilities
}

type pathChoice struct {
	target    string // the origin the configuration holds, or "auto"
	transport string // the mechanism the configuration holds, or "auto"
	label     string
	note      string
}

func (c pathChoice) selects(target, transport string) bool {
	return origin.Key(c.target) == origin.Key(target) && c.transport == transport
}

func automaticPath(note string) pathChoice {
	choice := pathChoice{target: "auto", transport: "auto", label: "Automatic"}
	if note != "" {
		choice.note = "→ " + note
	}
	return choice
}

func (m model) throughputPaths() []pathChoice {
	if m.multipleServers() {
		return m.sharedPaths(false)
	}
	resolved := ""
	if m.prepared != nil {
		resolved = shortOrigin(m.cfg.BaseURL, m.prepared.ThroughputTarget.Origin)
	}
	choices := []pathChoice{automaticPath(resolved)}
	seen := map[string]struct{}{}
	for _, t := range m.capabilities().ThroughputTargets {
		if t.Transport == wire.TransportWebTransportDatagram {
			continue
		}
		key := origin.Key(t.Origin) + "\x00" + t.Transport
		if _, ok := seen[key]; ok {
			continue
		}
		seen[key] = struct{}{}
		choices = append(choices, pathChoice{
			target:    t.Origin,
			transport: t.Transport,
			label:     goclient.ConnectionSummary(t.Transport, t.Protocol, t.TLS),
			note:      shortOrigin(m.cfg.BaseURL, t.Origin),
		})
	}
	return choices
}

func (m model) latencyPaths() []pathChoice {
	if m.multipleServers() {
		return m.sharedPaths(true)
	}
	resolved := ""
	if m.prepared != nil && m.prepared.LatencyTarget != nil {
		resolved = shortOrigin(m.cfg.BaseURL, m.prepared.LatencyTarget.Origin)
	}
	choices := []pathChoice{automaticPath(resolved)}
	seen := map[string]struct{}{}
	for _, t := range m.capabilities().LatencyTargets {
		key := origin.Key(t.Origin) + "\x00" + t.Transport
		if _, ok := seen[key]; ok {
			continue
		}
		seen[key] = struct{}{}
		choices = append(choices, pathChoice{
			target:    t.Origin,
			transport: t.Transport,
			label:     goclient.ConnectionSummary(t.Transport, t.Protocol, t.TLS),
			note:      shortOrigin(m.cfg.BaseURL, t.Origin),
		})
	}
	return choices
}

func (m model) selectedThroughputPath() *wire.ThroughputTarget {
	if m.cfg.ThroughputTarget == "auto" || m.cfg.ThroughputTransport == "auto" {
		return nil
	}
	targets := m.capabilities().ThroughputTargets
	i := slices.IndexFunc(targets, func(t wire.ThroughputTarget) bool {
		return t.Transport == m.cfg.ThroughputTransport && origin.Key(t.Origin) == origin.Key(m.cfg.ThroughputTarget)
	})
	if i >= 0 {
		return &targets[i]
	}
	return nil
}

func nextPath(target, transport string, choices []pathChoice) pathChoice {
	for i, choice := range choices {
		if choice.selects(target, transport) {
			return choices[(i+1)%len(choices)]
		}
	}
	return choices[0]
}

func nextChoice(current string, choices []string) string {
	for i, choice := range choices {
		if choice == current {
			return choices[(i+1)%len(choices)]
		}
	}
	return choices[0]
}
