package main

import (
	"context"
	"errors"
	"fmt"
	"strconv"
	"strings"
	"time"

	"github.com/charmbracelet/bubbles/cursor"
	"github.com/charmbracelet/bubbles/spinner"
	"github.com/charmbracelet/bubbles/textinput"
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
// a single result, so the model only advances this where a message carries the
// evidence: a challenge proves the server answered, a PreparationError proves
// the preflight body decoded, and only a finished preparation proves the
// target origins resolved.
type prepareStep int

const (
	stepReach prepareStep = iota
	stepPreflight
	stepOrigins
	stepReady
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

// connectionChecks is the readiness checklist for the current preparation
// attempt. Authorization is orthogonal to the handshake order: a server can
// demand it at the preflight request itself or at a later target probe.
func (m model) connectionChecks() []check {
	state := func(step prepareStep) checkState {
		switch {
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

type stageState int

const (
	stagePending stageState = iota
	stageWarmup
	stageMeasuring
	stageDone
	stageStopped
)

// stageProgress is one row of the run screen's timeline. duration is the
// configured measurement window, which the engine holds to exactly. The warmup
// window is stretched to the measured RTT inside the engine and is not
// reported, so a warming stage can only be timed by elapsed.
type stageProgress struct {
	name     string
	duration time.Duration
	state    stageState
	since    time.Time
}

// plannedStages is the enabled stages in the order the engine runs them.
func plannedStages(cfg goclient.Config) []stageProgress {
	var stages []stageProgress
	add := func(on bool, name string, d time.Duration) {
		if on {
			stages = append(stages, stageProgress{name: name, duration: d})
		}
	}
	add(cfg.Stages.Latency, "latency", cfg.LatencyDuration)
	add(cfg.Stages.Download, "download", cfg.DownloadDuration)
	add(cfg.Stages.Upload, "upload", cfg.UploadDuration)
	add(cfg.Stages.Bidirectional, "bidirectional", cfg.BidirectionalDuration)
	return stages
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
	lay     *layout
	spin    spinner.Model
	// now paces every elapsed clock on screen. The spinner's frame tick sets
	// it, so the clocks and the frames advance together.
	now time.Time

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
	prepareStep                         prepareStep
	prepareError                        string
	auth                                *goclient.PendingAuthorization
	authSince                           time.Time

	rates   map[goclient.Direction]goclient.ThroughputSample
	peaks   map[goclient.Direction]float64
	results []goclient.Result
	latency goclient.LatencySample
	stages  []stageProgress
}

func newModel(cfg goclient.Config) model {
	spin := spinner.New(spinner.WithSpinner(spinner.MiniDot))
	spin.Style = accentStyle
	return model{
		cfg:           cfg,
		mode:          modeConfigure,
		notice:        "Choose a server while the selected paths are checked.",
		prepareSeq:    1,
		prepareStatus: "checking",
		lay:           &layout{},
		spin:          spin,
		now:           time.Now(),
		rates:         map[goclient.Direction]goclient.ThroughputSample{},
		peaks:         map[goclient.Direction]float64{},
	}
}

func (m model) Init() tea.Cmd {
	return tea.Batch(prepareConnection(m.prepareSeq, m.cfg), m.spin.Tick)
}

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

// authWait is how long a browser approval may stay outstanding, and the
// deadline the configure screen counts down against.
const authWait = 2 * time.Minute

func pollAuthorization(seq int, p *goclient.PendingAuthorization) tea.Cmd {
	return func() tea.Msg {
		ctx, cancel := context.WithTimeout(context.Background(), authWait)
		defer cancel()
		token, err := p.Poll(ctx)
		return authTokenMsg{seq: seq, token: token, origin: p.Origin, err: err}
	}
}

func (m model) reprepare(cmd tea.Cmd) (tea.Model, tea.Cmd) {
	m.prepareSeq++
	m.prepareStatus = "checking"
	m.prepareStep = stepReach
	m.prepareError = ""
	m.prepared = nil
	m.auth = nil
	return m, tea.Batch(cmd, prepareConnection(m.prepareSeq, m.cfg), m.spin.Tick)
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
	case tea.MouseMsg:
		return m.handleMouse(msg)
	case spinner.TickMsg:
		if !m.animating() {
			return m, nil
		}
		m.now = msg.Time
		var cmd tea.Cmd
		m.spin, cmd = m.spin.Update(msg)
		return m, cmd
	case preparationMsg:
		if msg.seq != m.prepareSeq {
			return m, nil
		}
		var preparationErr *goclient.PreparationError
		preflightDecoded := errors.As(msg.err, &preparationErr)
		if preflightDecoded {
			pf := preparationErr.Preflight
			m.discovery = &pf
		}
		if msg.err != nil {
			var authErr *goclient.AuthRequiredError
			if errors.As(msg.err, &authErr) {
				m.prepareStatus = "authorizing"
				m.prepareStep = stepPreflight
				if preflightDecoded {
					m.prepareStep = stepOrigins
				}
				m.prepareError = ""
				m.notice = "Authentication is required. Preparing browser approval…"
				return m, tea.Batch(beginAuthorization(m.prepareSeq, m.cfg, authErr.URL), m.spin.Tick)
			}
			m.prepareStatus = "failed"
			m.prepareStep = stepReach
			if preflightDecoded {
				m.prepareStep = stepOrigins
			} else {
				m.discovery = nil
			}
			m.prepareError = msg.err.Error()
			m.prepared = nil
		} else {
			m.prepareStatus = "ready"
			m.prepareStep = stepReady
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
		m.auth = msg.pending
		m.authSince = time.Now()
		m.now = m.authSince
		m.notice = "Waiting for the browser approval."
		return m, tea.Batch(pollAuthorization(msg.seq, msg.pending), m.spin.Tick)
	case authTokenMsg:
		if msg.seq != m.prepareSeq {
			return m, nil
		}
		m.auth = nil
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
		m.now = time.Now()
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
			if m.prepareStep < stepPreflight {
				m.prepareStep = stepPreflight
			}
			m.notice = "Authentication expired. Preparing browser approval…"
			return m, tea.Batch(beginAuthorization(m.prepareSeq, m.cfg, authErr.URL), m.spin.Tick)
		}
		if msg.err != nil && !strings.Contains(msg.err.Error(), "context canceled") {
			m.err = msg.err
			m.status = "error"
		}
		if msg.err != nil && strings.Contains(msg.err.Error(), "context canceled") {
			m.status = "canceled"
		}
		m.stopStages()
		m.complete = true
		return m, nil
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
		return m.confirm()
	case "r":
		return m.startRun()
	case "v":
		return m.reprepare(nil)
	case "esc":
		m.notice = "Configuration kept. Press q to quit or r to run."
	default:
		// The custom URL row is a text field, so runes it receives while merely
		// selected open the editor carrying them. A bracketed paste arrives as
		// one rune batch and lands whole.
		if m.section == sectionServers && m.row == len(serverPresets) && msg.Type == tea.KeyRunes && !msg.Alt {
			m.edit = beginEdit(editURL, "url", string(msg.Runes))
			m.notice = "Editing server URL. Enter applies, esc cancels."
		}
	}
	return m, nil
}

// handleMouse resolves a click against the positions View recorded. A click on
// an unselected row only selects it; clicking the selected row activates it,
// which is what enter does and what opens a text field.
func (m model) handleMouse(msg tea.MouseMsg) (tea.Model, tea.Cmd) {
	if msg.Action != tea.MouseActionPress || msg.Button != tea.MouseButtonLeft {
		return m, nil
	}
	if m.mode != modeConfigure || m.edit.kind != editNone {
		return m, nil
	}
	if msg.Y == m.lay.tabY {
		for i, tab := range m.lay.tabs {
			if msg.X >= tab.from && msg.X < tab.to {
				m.section = section(i)
				m.row = clamp(m.row, 0, m.rowCount()-1)
				break
			}
		}
		return m, nil
	}
	if msg.X >= m.lay.rowRight {
		return m, nil
	}
	for i, y := range m.lay.rows {
		if y != msg.Y {
			continue
		}
		if i == m.row {
			return m.confirm()
		}
		m.row = i
		return m, nil
	}
	return m, nil
}

// confirm activates the selected row and re-checks the connection when that
// changed the configuration the current preparation was made for.
func (m model) confirm() (tea.Model, tea.Cmd) {
	before := m.cfg
	updated, cmd := m.activate()
	next := updated.(model)
	if next.mode == modeConfigure && next.edit.kind == editNone && next.cfg != before {
		return next.reprepare(cmd)
	}
	return next, cmd
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
	field := m.edit.field

	switch m.edit.kind {
	case editURL:
		if raw == "" {
			m.editRejected("Server URL cannot be empty.")
			return
		}
		if raw != m.cfg.BaseURL {
			m.cfg.AuthToken = ""
			m.cfg.AuthOrigin = ""
		}
		m.cfg.BaseURL = raw
		m.editAccepted("Custom server URL set.")
	case editDuration:
		d, err := time.ParseDuration(raw)
		if err != nil || d < 0 {
			m.editRejected("Use Go duration syntax, for example 800ms, 4s, or 1m.")
			return
		}
		switch field {
		case "warmup":
			m.cfg.Warmup = d
		case "latency":
			if d == 0 {
				m.editRejected("Latency duration must be greater than zero.")
				return
			}
			m.cfg.LatencyDuration = d
		case "download":
			if d == 0 {
				m.editRejected("Download duration must be greater than zero.")
				return
			}
			m.cfg.DownloadDuration = d
		case "upload":
			if d == 0 {
				m.editRejected("Upload duration must be greater than zero.")
				return
			}
			m.cfg.UploadDuration = d
		case "bidirectional":
			if d == 0 {
				m.editRejected("Bidirectional duration must be greater than zero.")
				return
			}
			m.cfg.BidirectionalDuration = d
		case "ping":
			if d == 0 {
				m.editRejected("Ping interval must be greater than zero.")
				return
			}
			m.cfg.PingInterval = d
		}
		m.editAccepted("Timing updated.")
	case editInt:
		n, err := strconv.Atoi(raw)
		min := 0
		if field == "auto-streams" {
			min = 1
		}
		if err != nil || n < min || n > 128 {
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
		m.edit = beginEdit(editDuration, timingFields[m.row], m.durationValue(m.row))
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
			m.edit = beginEdit(editInt, "auto-streams", fmt.Sprintf("%d", m.cfg.TransferStreams.AutomaticMax))
			m.notice = "Editing maximum automatic HTTP/1 streams. Use 1 through 128."
		case 4:
			m.edit = beginEdit(editInt, "streams", fmt.Sprintf("%d", m.cfg.TransferStreams.Forced))
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
	m.now = time.Now()
	m.rates = map[goclient.Direction]goclient.ThroughputSample{}
	m.peaks = map[goclient.Direction]float64{}
	m.results = nil
	m.latency = goclient.LatencySample{}
	m.stages = plannedStages(cfg)
	m.notice = "Run started. Press c or esc to cancel."
	return m, tea.Batch(waitEvents(events), waitDone(done), m.spin.Tick)
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
	case goclient.EventStage:
		m.stage = e.Stage
		m.status = e.Message
		m.enterStage(e)
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
			m.finishStage(e.Result.Stage)
		}
	case goclient.EventComplete:
		m.status = "complete"
		m.complete = true
	case goclient.EventError:
		m.err = e.Err
		m.status = "error"
		m.stopStages()
	}
}

// enterStage moves a timeline row into the phase the engine just announced.
// The engine names exactly two, so anything else leaves the row where it is.
func (m *model) enterStage(e goclient.Event) {
	var state stageState
	switch e.Message {
	case "warmup":
		state = stageWarmup
	case "measure":
		state = stageMeasuring
	default:
		return
	}
	for i := range m.stages {
		if m.stages[i].name == e.Stage {
			m.stages[i].state, m.stages[i].since = state, e.At
		}
	}
}

// finishStage closes a timeline row. A stage emits its results only once every
// lane has stopped, so the first one proves the measurement window closed; the
// rest of the stage's results land on a row that is already done.
func (m *model) finishStage(stage string) {
	for i := range m.stages {
		if m.stages[i].name == stage && m.stages[i].state != stagePending {
			m.stages[i].state = stageDone
		}
	}
}

// stopStages marks whichever stage was running when the run ended early, so a
// canceled or failed run does not leave a row spinning forever.
func (m *model) stopStages() {
	for i := range m.stages {
		if s := m.stages[i].state; s == stageWarmup || s == stageMeasuring {
			m.stages[i].state = stageStopped
		}
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
		return 7
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
