package main

import (
	"context"
	"fmt"
	"net/url"
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

// eventsMsg and doneMsg carry the sequence of the run that produced them. A
// batch read from a finished run's channel cannot land in a later run's model.
// preparationMsg carries the same guard as prepareSeq.
type eventsMsg struct {
	seq    int
	events []goclient.Event
}
type doneMsg struct {
	seq int
	err error
}
type preparationMsg struct {
	seq        int
	connection *goclient.PreparedConnection
	err        error
}

// prepareDueMsg is the debounce timer of the preparation it carries the
// sequence of. A later configuration change bumps the sequence, so the timer of
// a superseded change fires into nothing.
type prepareDueMsg struct{ seq int }

// Both auth messages carry the prepareSeq of the preparation that starts the
// authorization. A browser approval stays outstanding for up to two minutes.
// The sequence stops a poll detached by a server switch from overwriting a
// newer preparation.
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

// The Connections rows, in the order networkView draws them. Named because the
// activation switch, the row count, and the two editors that render in place
// all have to agree on which row is which.
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

// protocolNegotiated is the protocol an origin advertises when it fixes no HTTP
// version — the one case where the version row still has a decision to make.
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

// beginEdit seeds a focused field with value. The cursor is static because
// nothing else in this program drives redraws from a blink command.
func beginEdit(kind editKind, field, value string) editState {
	in := textinput.New()
	in.Prompt = ""
	in.TextStyle = valueStyle
	in.Cursor.SetMode(cursor.CursorStatic)
	in.SetValue(value)
	in.Focus()
	return editState{kind: kind, field: field, input: in}
}

// prepareStep is the first step of the connection handshake the current
// preparation attempt has not proven. Prepare answers the whole handshake with
// a single result, so the model advances this only where a message carries the
// evidence named beside each step.
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

// statusIdle is where a launched client sits until the operator points it at a
// server. Nothing is dialled and nothing is claimed about the path, so the
// screen carries no result — good or bad — that the operator did not ask for.
const statusIdle = "not checked"

// connectionChecks is the readiness checklist for the current preparation
// attempt. Authorization is orthogonal to the handshake order: a server can
// demand it at the preflight request itself or at a later target probe.
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
	cfg   goclient.Config
	mode  mode
	width int

	section section
	row     int
	edit    editState
	notice  string
	spin    spinner.Model
	help    help.Model
	// cancelPrompt is the run screen waiting for the second esc that stops the
	// run. Any other key clears it and the run carries on.
	cancelPrompt bool
	// now paces every elapsed clock on screen. The spinner's frame tick sets
	// it, so the clocks and the frames advance together.
	now time.Time

	// runSeq stamps every message a run emits; a superseded run's messages
	// carry an older sequence and are dropped.
	runSeq int
	events <-chan goclient.Event
	done   <-chan error
	cancel context.CancelFunc

	// displayRates trail rates toward each authoritative sample on the
	// animation tick, so the live bars and figures glide instead of jumping
	// with every 100ms sample.
	displayRates map[goclient.Direction]float64
	// lostStreak counts consecutive lost pings; a good pong resets it. The
	// latency line keeps its last value against a short streak instead of
	// blinking between figure and timeout.
	lostStreak int

	stage                               string
	status                              string
	server                              string
	target, latencyTarget               string
	throughputProtocol, latencyProtocol string
	// The transports the run committed to: the stream label names what a session
	// delivers rather than what was requested, and the latency line names the
	// bus, since loss means different things on each.
	throughputTransport, latencyTransport string

	err           error
	complete      bool
	prepared      *goclient.PreparedConnection
	discovery     *wire.Preflight
	prepareSeq    int
	prepareStatus string
	prepareStep   prepareStep
	prepareError  string
	auth          *goclient.PendingAuthorization
	authSince     time.Time
	// authOpened records that the approval page was sent to the browser. Until
	// it is set, enter is the key that sends it; afterwards enter goes back to
	// the row it belongs to.
	authOpened bool
	// openApproval launches the browser. It is a field so a test can watch the
	// call instead of opening a window.
	openApproval func(*goclient.PendingAuthorization)

	rates   map[goclient.Direction]goclient.ThroughputSample
	peaks   map[goclient.Direction]float64
	results []goclient.Result
	latency goclient.LatencySample
	stages  []stageProgress
}

// animationFPS paces the spinner frames, the elapsed clocks, and the rate
// glide, all of which advance on the spinner's frame tick.
const animationFPS = 20

func newModel(cfg goclient.Config) model {
	dial := spinner.MiniDot
	dial.FPS = time.Second / animationFPS
	spin := spinner.New(spinner.WithSpinner(dial))
	spin.Style = accentStyle
	return model{
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

// Init draws the configure screen and waits. A launch reaches no server on its
// own: the URL a client starts with is a default, not a destination, and
// dialling it unasked answers with a failure the operator never requested.
func (m model) Init() tea.Cmd { return nil }

// animating reports whether the screen changes without an incoming message:
// the spinner frames and the elapsed clocks. Anything that enters one of these
// states restarts the frame tick; duplicate starts are absorbed by the
// spinner's own tag guard.
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
	case doneMsg:
		return m.handleDone(msg)
	default:
		// The clipboard read behind ctrl+v answers with a message only the text
		// input understands.
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
	case m.edit.kind != editNone:
		return m.handleEditKey(msg)
	case m.auth != nil && !m.authOpened && key.Matches(msg, keys.approve):
		m.openApproval(m.auth)
		m.authOpened = true
		m.notice = "Approval page opened in the browser."
		return m, nil
	case m.urlRowRune(msg):
		// A bracketed paste arrives as one rune message and lands whole.
		m.edit = beginEdit(editURL, "url", string(msg.Runes))
		m.notice = "Editing server URL. Enter applies, esc cancels."
		return m, nil
	case key.Matches(msg, keys.quit):
		if m.cancel != nil {
			m.cancel()
		}
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

// urlRowRune reports whether msg is printable text typed or pasted on the
// selected Custom URL row. There every rune is text, not a binding: a hostname
// may start with r, q, or j. ctrl+c is not a rune, so quit and the navigation
// keys still reach their handlers.
func (m model) urlRowRune(msg tea.KeyMsg) bool {
	return m.mode == modeConfigure && !m.cancelPrompt &&
		m.section == sectionServers && m.row == len(serverPresets) &&
		msg.Type == tea.KeyRunes && !msg.Alt
}

func (m model) handleRunKey(msg tea.KeyMsg) (tea.Model, tea.Cmd) {
	if !m.complete {
		if key.Matches(msg, keys.cancel) && m.cancel != nil {
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
	if m.cancel != nil {
		m.cancel()
		m.status = "canceling"
	}
	m.notice = "Canceling the run."
	return m, nil
}

// focusServer puts the configure screen on the server the current URL names.
// The server selection is what asks a server for authorization, so an approval
// prompt lands beside the row that caused it. An open editor keeps the cursor:
// the operator is mid-entry and the section is already on show.
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

// confirm activates the selected row. A configuration edit invalidates the
// current preparation, so it starts a fresh connection check. Picking a server
// starts one whether or not the URL changed: that row is how an idle client is
// pointed at something, and re-picking the server it already holds is a request
// to check it.
func (m model) confirm() (tea.Model, tea.Cmd) {
	before := m.cfg
	updated, cmd := m.activate()
	next := updated.(model)
	if next.mode != modeConfigure || next.edit.kind != editNone {
		return next, cmd
	}
	if next.cfg != before || (m.section == sectionServers && m.row < len(serverPresets)) {
		return next.reprepare(cmd)
	}
	return next, cmd
}

func (m model) handleEditKey(msg tea.KeyMsg) (tea.Model, tea.Cmd) {
	switch {
	case key.Matches(msg, keys.abort):
		if m.cancel != nil {
			m.cancel()
		}
		return m, tea.Quit
	case key.Matches(msg, keys.discard):
		m.notice = "Edit canceled."
		m.edit = editState{}
		return m, nil
	case key.Matches(msg, keys.apply):
		before, wasURL := m.cfg, m.edit.kind == editURL
		m.commitEdit()
		// Applying the URL editor is the other way to point an idle client at a
		// server, so it checks even when the same URL is retyped.
		if m.cfg != before || (wasURL && m.edit.kind == editNone) {
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
	// A scheme-less entry is completed, never upgraded: what is typed is what
	// is dialed. The scheme alone decides the port when none is given, which is
	// http's 80 and https's 443.
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
	// warmup may be zero; every stage window and the ping interval must be
	// positive. An empty zeroError marks the fields that accept zero.
	type slot struct {
		ptr       *time.Duration
		zeroError string
	}
	slots := map[string]slot{
		"warmup":        {&m.cfg.Warmup, ""},
		"latency":       {&m.cfg.LatencyDuration, "Latency duration must be greater than zero."},
		"download":      {&m.cfg.DownloadDuration, "Download duration must be greater than zero."},
		"upload":        {&m.cfg.UploadDuration, "Upload duration must be greater than zero."},
		"bidirectional": {&m.cfg.BidirectionalDuration, "Bidirectional duration must be greater than zero."},
		"ping":          {&m.cfg.PingInterval, "Ping interval must be greater than zero."},
	}
	if s, ok := slots[field]; ok {
		if d == 0 && s.zeroError != "" {
			m.editRejected(s.zeroError)
			return
		}
		// The ping cadence has an upper bound over the datagram bus, which the
		// server reaps once the client stops feeding it. The bound is the
		// client's one statement of that rule, message included; the WebSocket
		// bus has no idle timer, and automatic selection defers the decision to
		// Prepare so an unreachable datagram bus can fall back first.
		if field == "ping" && goclient.PingIntervalBoundApplies(m.cfg.LatencyTransport) {
			if err := goclient.ValidatePingInterval(d); err != nil {
				m.editRejected(err.Error())
				return
			}
		}
		*s.ptr = d
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
		m.edit = beginEdit(editDuration, timingFields[m.row], m.durationValue(m.row))
		m.notice = "Editing duration. Use values like 800ms, 4s, or 1m."
	case sectionConnections:
		switch m.row {
		case rowThroughputPath:
			next := nextPath(m.cfg.ThroughputTarget, m.cfg.ThroughputTransport, m.throughputPaths())
			m.cfg.ThroughputTarget, m.cfg.ThroughputTransport = next.target, next.transport
			// A path that fixes its HTTP version leaves a pinned one from the
			// previous selection stranded, where it fails the check with
			// "endpoint is fixed to http1, cannot use http2" and points at a row
			// that now reads as inert. Selecting the path releases the pin.
			if t := m.selectedThroughputPath(); t != nil && t.Protocol != protocolNegotiated {
				m.cfg.ThroughputProtocol = "auto"
			}
			m.notice = "Throughput path set to " + next.label + "."
		case rowThroughputProtocol:
			if t := m.selectedThroughputPath(); t != nil && t.Protocol != protocolNegotiated {
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
			// The editor still opens: the value is kept for the transports that
			// read it, so it stays settable while WebTransport is selected.
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
		return 5
	case sectionTiming:
		return 6
	case sectionConnections:
		return rowConnectionsCount
	case sectionRun:
		return 1
	default:
		return 1
	}
}

// timingFields names the duration each Timing row edits, in row order.
var timingFields = []string{"warmup", "latency", "download", "upload", "bidirectional", "ping"}

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

// pathChoice is one stop of a path row's cycle. Origin and mechanism move
// together: pulled apart they spell combinations no server offers — WebTransport
// over an HTTP/1.1 listener — and the operator finds out when the check fails.
// Descriptions come from discovery, which the model keeps across configuration
// changes, so a row says what it offers without waiting for a connection.
type pathChoice struct {
	target    string // the origin the configuration holds, or "auto"
	transport string // the mechanism the configuration holds, or "auto"
	label     string
	note      string
}

// selects reports whether this stop is the one the configuration currently
// holds. Discovery can advertise one origin under several spellings, so
// origin.Key decides equivalence.
func (c pathChoice) selects(target, transport string) bool {
	return origin.Key(c.target) == origin.Key(target) && c.transport == transport
}

// automaticPath is the first stop of every path cycle: the server decides. Its
// note names where the last check resolved it, which the model keeps across
// configuration changes and so never blanks mid-cycle.
func automaticPath(note string) pathChoice {
	choice := pathChoice{target: "auto", transport: "auto", label: "Automatic"}
	if note != "" {
		choice.note = "→ " + note
	}
	return choice
}

// throughputPaths and latencyPaths are what the two path rows cycle through:
// automatic, then one stop per advertised (origin, mechanism) pair, each named
// the way the readiness panel and the run screen name the path it commits to.
func (m model) throughputPaths() []pathChoice {
	resolved := ""
	if m.prepared != nil {
		resolved = shortOrigin(m.cfg.BaseURL, m.prepared.ThroughputTarget.Origin)
	}
	choices := []pathChoice{automaticPath(resolved)}
	seen := map[string]struct{}{}
	for _, t := range m.capabilities().ThroughputTargets {
		// The datagram flood is a browser-side loss diagnostic; selectTarget
		// refuses it outright, so it is not offered as something to pick.
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

// selectedThroughputPath is the advertised target the configuration names, or
// nil where it names automatic, something discovery has not offered, or nothing
// discovered yet. It is what tells the HTTP-version row whether it has a
// decision left to make.
func (m model) selectedThroughputPath() *wire.ThroughputTarget {
	if m.cfg.ThroughputTarget == "auto" || m.cfg.ThroughputTransport == "auto" {
		return nil
	}
	targets := m.capabilities().ThroughputTargets
	for i := range targets {
		t := &targets[i]
		if t.Transport == m.cfg.ThroughputTransport && origin.Key(t.Origin) == origin.Key(m.cfg.ThroughputTarget) {
			return t
		}
	}
	return nil
}

// nextPath walks the cycle from wherever the configuration sits. A pair no stop
// carries — a flag combination, or a selection whose origin has left discovery
// — resumes at automatic rather than at an arbitrary stop.
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
