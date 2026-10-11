package main

import (
	"errors"
	"fmt"
	"net"
	"net/url"
	"slices"
	"strconv"
	"strings"
	"time"

	"charm.land/bubbles/v2/key"
	"charm.land/bubbles/v2/textinput"
	tea "charm.land/bubbletea/v2"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// The settings sheet's rows: what each shows and says, grouped as the browser's settings are, and what their keys
// change.

type setupRow struct {
	label, value, help string
	inert              bool
}

type setting struct {
	label, help string
	flag        func(*goclient.Config) *bool
	span        func(*goclient.Config) *time.Duration
	view        func(model) setupRow
	act         func(*model)
	cycle       func(m *model, step int)
	parse       func(*model, string) error
}

func (s *setting) row(m model) setupRow {
	switch {
	case s.view != nil:
		return s.view(m)
	case s.flag != nil && s.span != nil:
		return setupRow{label: s.label, value: fmtSetting(*s.span(&m.cfg)), help: s.help, inert: !*s.flag(&m.cfg)}
	case s.span != nil:
		return setupRow{label: s.label, value: fmtSetting(*s.span(&m.cfg)), help: s.help}
	}
	return setupRow{label: s.label, help: s.help}
}

func stageSetting(label, what string, flag func(*goclient.Config) *bool,
	span func(*goclient.Config) *time.Duration) *setting {
	return &setting{label: label, flag: flag, span: span, help: what}
}

var (
	catalogueRow = &setting{
		view: func(m model) setupRow {
			value := m.cfg.BaseURL
			if value == "" {
				value = "none yet"
			}
			return setupRow{label: "Address", value: value, help: "The Graphite Meter server to test, which lists " +
				"its test servers. enter edits it; a pasted address works too."}
		},
		parse: func(m *model, raw string) error {
			canonical, err := serverOrigin(raw)
			if err != nil {
				return err
			}
			if canonical != m.cfg.BaseURL {
				m.cfg.ServerIDs = nil
			}
			m.cfg.BaseURL = canonical
			return nil
		},
	}
	rememberRow = &setting{label: "Remember server",
		help: "Keeps the address on this computer, so the next start needs none. Off deletes it."}
	serversRow = &setting{view: func(m model) setupRow {
		value := m.selectedServerNames()
		if summary := m.readinessSummary(); summary != "" {
			value += " · " + summary
		}
		return setupRow{label: "Test servers", value: value, inert: !m.canChooseServers(), help: fmt.Sprintf(
			"Measured at once; their speeds add up. enter picks up to %d.", wire.MaxSelectedServers)}
	}}
	throughputPathRow = pathSetting("Throughput path", false, func(c *goclient.Config) (*string, *string) {
		return &c.ThroughputTarget, &c.ThroughputTransport
	})
	latencyPathRow = pathSetting("Latency path", true, func(c *goclient.Config) (*string, *string) {
		return &c.LatencyTarget, &c.LatencyTransport
	})
	protocolRow = &setting{
		view: protocolView,
		cycle: func(m *model, step int) {
			if row := protocolView(*m); !row.inert {
				m.cfg.ThroughputProtocol = nextChoice(m.cfg.ThroughputProtocol, []string{"auto", "http1", "http2", "http3"},
					step)
			}
		},
	}
	stageRows = []*setting{
		stageSetting("Latency", "Idle round trips.",
			func(c *goclient.Config) *bool { return &c.Stages.Latency },
			func(c *goclient.Config) *time.Duration { return &c.LatencyDuration }),
		stageSetting("Download", "Server to client.",
			func(c *goclient.Config) *bool { return &c.Stages.Download },
			func(c *goclient.Config) *time.Duration { return &c.DownloadDuration }),
		stageSetting("Upload", "Client to server, timed by the server as it receives.",
			func(c *goclient.Config) *bool { return &c.Stages.Upload },
			func(c *goclient.Config) *time.Duration { return &c.UploadDuration }),
		stageSetting("Bidirectional", "Download and upload at once.",
			func(c *goclient.Config) *bool { return &c.Stages.Bidirectional },
			func(c *goclient.Config) *time.Duration { return &c.BidirectionalDuration }),
	}
	loadedLatencyRow = &setting{label: "Loaded latency", flag: func(c *goclient.Config) *bool { return &c.LoadedLatency },
		help: "Round trips during transfers: the latency the load adds."}
	warmupRow = &setting{label: "Warmup", span: func(c *goclient.Config) *time.Duration { return &c.Warmup },
		help: fmt.Sprintf("Ramp-up before each window, at least ten round trips. ←→ ±100 ms, from %s to %s.",
			fmtSetting(goclient.WarmupBound.Min), fmtSetting(goclient.WarmupBound.Max))}
	idleCadenceRow = cadenceSetting("Idle probe spacing",
		"How often idle latency is probed: on each reply, or every 80, 250 or 600 ms.",
		func(c *goclient.Config) *time.Duration { return &c.PingInterval })
	loadedCadenceRow = cadenceSetting("Loaded probe spacing", "How often latency is probed during transfers.",
		func(c *goclient.Config) *time.Duration { return &c.LoadedPingInterval })
	forceStreamsRow = &setting{
		label: "Exact stream count",
		help:  "Off: each path picks its own. On: the count below everywhere.",
		view: func(m model) setupRow {
			return setupRow{label: "Exact stream count",
				help: "Off: each path picks its own. On: the count below everywhere."}
		},
		cycle: func(m *model, _ int) {
			streams := &m.cfg.TransferStreams
			if streams.Forced > 0 {
				streams.Forced = 0
			} else {
				streams.Forced = streams.AutomaticMax
			}
		},
	}
	streamsRow = &setting{
		view: func(m model) setupRow {
			row := setupRow{label: "HTTP/1.1 streams", value: strconv.Itoa(*streamCount(&m.cfg)),
				help: fmt.Sprintf("The most HTTP/1.1 connections per direction. ←→ ±1, from 1 to %d.", goclient.MaxStreams)}
			if m.cfg.TransferStreams.Forced > 0 {
				row.label = "Streams per direction"
			}
			return row
		},
		cycle: func(m *model, step int) {
			n := streamCount(&m.cfg)
			*n = min(max(*n+step, 1), goclient.MaxStreams)
		},
		parse: func(m *model, raw string) error {
			n, err := strconv.Atoi(raw)
			if err != nil || n < 1 || n > goclient.MaxStreams {
				return fmt.Errorf("streams must be a whole number from 1 to %d", goclient.MaxStreams)
			}
			*streamCount(&m.cfg) = n
			return nil
		},
	}
	insecureRow = &setting{label: "Skip TLS verification",
		flag: func(c *goclient.Config) *bool { return &c.InsecureSkipTLSVerify },
		help: "Accepts any certificate. Unsafe, and sign-in is refused."}
	resetRow = &setting{label: "Reset settings", help: "Restores the defaults; keeps the server and its test servers.",
		act: func(m *model) {
			if !m.resetPrompt {
				m.resetPrompt = true
				return
			}
			defaults := goclient.DefaultConfig()
			defaults.BaseURL, defaults.ServerIDs = m.cfg.BaseURL, m.cfg.ServerIDs
			m.cfg, m.resetPrompt = defaults, false
			*m = m.notify(styled("Settings reset to their defaults.", m.pal.muted))
		}}
)

func streamCount(c *goclient.Config) *int {
	if c.TransferStreams.Forced > 0 {
		return &c.TransferStreams.Forced
	}
	return &c.TransferStreams.AutomaticMax
}

// setupGroups are the sheet's rows under their headings.
var setupGroups = []struct {
	label string
	rows  []*setting
}{
	{"Server", []*setting{catalogueRow, rememberRow, serversRow, throughputPathRow, protocolRow, latencyPathRow}},
	{"Stages", append(slices.Clone(stageRows), loadedLatencyRow)},
	{"Advanced", []*setting{warmupRow, idleCadenceRow, loadedCadenceRow, forceStreamsRow, streamsRow, insecureRow,
		resetRow}},
}

func (m model) rows() []*setting {
	var rows []*setting
	for _, g := range setupGroups {
		rows = append(rows, g.rows...)
	}
	return rows
}

func (m model) currentRow() *setting { return m.rows()[min(m.row, len(m.rows())-1)] }

// flagOf is the switch a row turns on or off, if any.
func (m model) flagOf(s *setting) (bool, bool) {
	switch {
	case s.flag != nil:
		return *s.flag(&m.cfg), true
	case s == forceStreamsRow:
		return m.cfg.TransferStreams.Forced > 0, true
	case s == rememberRow:
		return m.remembering, true
	}
	return false, false
}

// openSettings opens the settings sheet, focused on row when given.
func (m model) openSettings(row *setting) model {
	if i := slices.Index(m.rows(), row); i >= 0 {
		m.row = i
	}
	m.overlay, m.edit, m.scroll = overlaySettings, nil, 0
	return m
}

// moveFocus moves the focus step rows.
func (m *model) moveFocus(step int) {
	m.row, m.resetPrompt = min(max(m.row+step, 0), len(m.rows())-1), false
}

// switchMark is a switch: on in ink, off as a quiet ring.
func (m model) switchMark(on bool) line {
	if on {
		return styled("● on ", m.pal.accent)
	}
	return styled("○ off", m.pal.muted)
}

// planTime is how long the configured stages take with their warmups.
func (m model) planTime() string {
	var total time.Duration
	for _, stage := range m.cfg.Plan() {
		total += stage.Duration + m.cfg.Warmup
	}
	return fmtSetting(total.Round(time.Second))
}

// settingsEntries are the sheet's entries and the focused row's index among them.
func (m model) settingsEntries() ([]entry, int) {
	var entries []entry
	focus, index := 0, 0
	for _, g := range setupGroups {
		entries = append(entries, entry{heading: g.label})
		for _, s := range g.rows {
			if index == m.row {
				focus = len(entries)
			}
			row := s.row(m)
			st := m.pal.value
			if row.inert {
				st = m.pal.muted
			}
			value := styled(row.value, st)
			if s == resetRow && m.resetPrompt {
				value = styled("enter again resets", m.pal.warn)
			}
			if on, ok := m.flagOf(s); ok {
				if row.value == "" {
					value = m.switchMark(on)
				} else {
					value = value.and("  ", style{}).with(m.switchMark(on))
				}
			}
			if m.edit != nil && m.edit.row == s {
				value = m.editLine(40)
			}
			entries = append(entries, entry{item: true, label: styled(row.label, m.pal.text), value: value,
				target: target{kind: targetRow, index: index}})
			index++
		}
	}
	return entries, focus
}

// settingsFooter is what the sheet's foot says: the editor's error, or what the focused row does.
func (m model) settingsFooter() []line {
	if m.edit != nil && m.edit.err != "" {
		return []line{styled(m.edit.err, m.pal.warn)}
	}
	s := m.currentRow()
	help := s.row(m).help
	switch {
	case s == throughputPathRow || s == latencyPathRow:
		help += fmt.Sprintf(" ←→ picks one of %d.", len(m.pathChoices(s == latencyPathRow)))
	case s.flag != nil && s.span != nil:
		help += " ←→ steps its time, space switches it."
	case s.flag != nil || s == forceStreamsRow || s == rememberRow:
		help += " space switches it."
	}
	return []line{styled(help, m.pal.muted)}
}

// handleSettingsKey reacts to an action on the focused row; a change to what a check depends on rechecks the paths.
func (m model) handleSettingsKey(a action, msg tea.KeyPressMsg) (tea.Model, tea.Cmd) {
	s := m.currentRow()
	confirmed := m.resetPrompt
	m.resetPrompt = false
	switch {
	case s == rememberRow && a == actAdjust:
		m.setRemembering(delta(msg) > 0)
		return m, nil
	case s == rememberRow && (a == actToggle || a == actOpen):
		m.setRemembering(!m.remembering)
		return m, nil
	}
	switch a {
	case actMove:
		m.moveFocus(delta(msg))
		return m, nil
	case actAdjust:
		return m.adjust(s, delta(msg))
	case actToggle:
		if on, ok := m.flagOf(s); ok {
			before := m.cfg
			m.setFlag(s, !on)
			return m.recheckIfPathsChanged(before)
		}
		return m.activate(s, confirmed)
	case actOpen:
		return m.activate(s, confirmed)
	}
	return m, nil
}

// clickRow is a click on row i: the first focuses it, a second does what space does.
func (m model) clickRow(i int) (tea.Model, tea.Cmd) {
	if m.row != i {
		m.row, m.resetPrompt = i, false
		return m, nil
	}
	return m.handleSettingsKey(actToggle, tea.KeyPressMsg{})
}

func protocolView(m model) setupRow {
	if t := m.selectedThroughputPath(); t != nil && t.Protocol != "negotiated" {
		return setupRow{label: "HTTP version", value: protocolLabel(t.Protocol), inert: true,
			help: "Fixed by this path; pick another path to change it."}
	}
	return setupRow{label: "HTTP version", value: protocolLabel(m.cfg.ThroughputProtocol),
		help: "Where the path negotiates. ←→ Automatic, HTTP/1.1, HTTP/2, HTTP/3."}
}

// activate is enter on s: open, edit, reset once confirmed, or step a choice.
func (m model) activate(s *setting, confirmed bool) (tea.Model, tea.Cmd) {
	before := m.cfg
	switch {
	case s == serversRow:
		return m.openServerChooser()
	case s == resetRow:
		m.resetPrompt = confirmed
		s.act(&m)
	case s.span != nil:
		m.beginEdit(s, fmtSetting(*s.span(&m.cfg)))
	case s.parse != nil:
		value := s.row(m).value
		if s == catalogueRow {
			value = m.cfg.BaseURL
		}
		m.beginEdit(s, value)
	case s.flag != nil:
		m.setFlag(s, !*s.flag(&m.cfg))
	case s.cycle != nil:
		s.cycle(&m, 1)
	}
	return m.recheckIfPathsChanged(before)
}

func (m model) adjust(s *setting, step int) (tea.Model, tea.Cmd) {
	before := m.cfg
	switch {
	case s.cycle != nil:
		s.cycle(&m, step)
	case s.span != nil:
		bound, unit := s.bound()
		d := s.span(&m.cfg)
		if s != warmupRow {
			// Steps grow with the stage, so the arrows reach an hour as readily as a second.
			unit = stageStep(*d - time.Duration(max(0, -step)))
		}
		*d = min(max(*d+time.Duration(step)*unit, bound.Min), bound.Max)
	case s.flag != nil:
		m.setFlag(s, step > 0)
	}
	return m.recheckIfPathsChanged(before)
}

func (m *model) setFlag(s *setting, on bool) {
	switch {
	case s.flag != nil:
		*s.flag(&m.cfg) = on
	case s == forceStreamsRow && (m.cfg.TransferStreams.Forced > 0) != on:
		s.cycle(m, 1)
	}
}

func shortOrigin(base, target string) string {
	u, err := url.Parse(target)
	if err != nil || u.Host == "" {
		return target
	}
	if b, err := url.Parse(base); err == nil && u.Port() != "" && strings.EqualFold(b.Hostname(), u.Hostname()) {
		return ":" + u.Port()
	}
	return u.Host
}

func (m model) recheckIfPathsChanged(before goclient.Config) (tea.Model, tea.Cmd) {
	if before.PreparationKey() == m.cfg.PreparationKey() {
		return m, nil
	}
	return m.reprepare()
}

type editState struct {
	row   *setting
	input textinput.Model
	err   string
}

func (m *model) beginEdit(s *setting, value string) {
	in := textinput.New()
	in.Prompt = ""
	in.SetValue(value)
	in.Focus()
	m.edit = &editState{row: s, input: in}
}

// editLine is the text being typed in w cells, the cursor a block over its character.
func (m model) editLine(w int) line {
	value := []rune(m.edit.input.Value())
	pos := min(max(m.edit.input.Position(), 0), len(value))
	under, after := " ", ""
	if pos < len(value) {
		under, after = string(value[pos]), string(value[pos+1:])
	}
	start := max(pos-(max(w, 2)-1), 0)
	return styled(string(value[start:pos]), m.pal.value).and(under, m.pal.cursor).and(after, m.pal.value).fit(w)
}

// handleEditKey is a key while typing: enter applies, esc cancels, others edit the text. The address being connected
// stays open while its check runs, and closes once it prepared.
func (m model) handleEditKey(msg tea.KeyPressMsg) (tea.Model, tea.Cmd) {
	switch {
	case m.overlay == overlayConnect && msg.String() == "tab":
		m.setRemembering(!m.remembering)
		return m, nil
	case key.Matches(msg, keys.discard):
		m.edit = nil
		if m.overlay == overlayConnect {
			m.overlay = overlayNone
		}
		return m, nil
	case key.Matches(msg, keys.apply):
		before := m.cfg
		if err := m.commitEdit(); err != nil {
			text := err.Error()
			m.edit = &editState{row: m.edit.row, input: m.edit.input, err: strings.ToUpper(text[:1]) + text[1:] + "."}
			return m, nil
		}
		if m.overlay == overlayConnect {
			return m.reprepareNow()
		}
		m.edit = nil
		return m.recheckIfPathsChanged(before)
	}
	return m.updateEdit(msg)
}

func (m model) updateEdit(msg tea.Msg) (tea.Model, tea.Cmd) {
	next := *m.edit
	next.err = ""
	var cmd tea.Cmd
	next.input, cmd = next.input.Update(msg)
	m.edit = &next
	return m, cmd
}

func (m *model) commitEdit() error {
	raw, s := strings.TrimSpace(m.edit.input.Value()), m.edit.row
	if s.span == nil {
		return s.parse(m, raw)
	}
	if n, err := strconv.ParseFloat(raw, 64); err == nil {
		raw = fmt.Sprintf("%gs", n)
	}
	d, err := time.ParseDuration(raw)
	if err != nil {
		return errors.New("use a time like 800ms, 4s or 1m; a bare number is seconds")
	}
	if err := s.inBounds(d); err != nil {
		return err
	}
	*s.span(&m.cfg) = d
	return nil
}

func stageStep(d time.Duration) time.Duration {
	switch {
	case d < time.Minute:
		return time.Second
	case d < 10*time.Minute:
		return 10 * time.Second
	case d < time.Hour:
		return time.Minute
	}
	return 5 * time.Minute
}

func (s *setting) bound() (goclient.DurationBound, time.Duration) {
	if s == warmupRow {
		return goclient.WarmupBound, 100 * time.Millisecond
	}
	return goclient.StageBound, time.Second
}

func (s *setting) inBounds(d time.Duration) error {
	bound, _ := s.bound()
	if err := bound.Check(d); err != nil {
		return fmt.Errorf("%s %w", s.label, err)
	}
	return nil
}

func cadenceSetting(label, help string, field func(*goclient.Config) *time.Duration) *setting {
	return &setting{
		view: func(m model) setupRow {
			return setupRow{label: label, value: cadenceLabel(*field(&m.cfg)), help: help}
		},
		cycle: func(m *model, step int) {
			interval := field(&m.cfg)
			*interval = cadences[(cadenceIndex(*interval)+step+len(cadences))%len(cadences)].interval
		},
	}
}

func pathSetting(label string, latency bool, field func(*goclient.Config) (*string, *string)) *setting {
	return &setting{
		view: func(m model) setupRow {
			target, transport := field(&m.cfg)
			return m.pathRow(label, *target, *transport, latency)
		},
		cycle: func(m *model, step int) {
			target, transport := field(&m.cfg)
			next := nextPath(*target, *transport, m.pathChoices(latency), step)
			*target, *transport = next.target, next.transport
			if t := m.selectedThroughputPath(); !latency && t != nil && t.Protocol != "negotiated" {
				m.cfg.ThroughputProtocol = "auto"
			}
		},
	}
}

type pathChoice struct {
	target    string
	transport string
	label     string
	note      string
}

func (c pathChoice) selects(target, transport string) bool {
	return (c.target == target || wire.SameOrigin(c.target, target)) && c.transport == transport
}

func (m model) pathRow(label, target, transport string, latency bool) setupRow {
	choices := m.pathChoices(latency)
	help := map[bool]string{false: "How transfers reach the server.", true: "How probes travel."}[latency]
	for _, choice := range choices {
		if choice.selects(target, transport) {
			value := choice.label
			if choice.note != "" {
				value += " · " + choice.note
			}
			return setupRow{label: label, value: value, help: help}
		}
	}
	mechanism := transportLabel(transport, latency)
	value := mechanism + " · " + target
	if target == "auto" {
		value = mechanism + " · automatic origin"
	}
	return setupRow{label: label, value: value + " (not offered)", help: help}
}

func nextPath(target, transport string, choices []pathChoice, step int) pathChoice {
	for i, choice := range choices {
		if choice.selects(target, transport) {
			return choices[(i+step+len(choices))%len(choices)]
		}
	}
	return choices[0]
}

func nextChoice(current string, choices []string, step int) string {
	return choices[(slices.Index(choices, current)+step+len(choices))%len(choices)]
}

func discovery(server goclient.PreparedServer) *wire.Preflight {
	if server.Connection != nil {
		return &server.Connection.Preflight
	}
	if failed, ok := errors.AsType[*goclient.PreparationError](server.Err); ok {
		return &failed.Preflight
	}
	return nil
}

func (m model) singleDiscovery() *wire.Preflight {
	if m.preparedRun == nil || len(m.preparedRun.Servers) != 1 {
		return nil
	}
	return discovery(m.preparedRun.Servers[0])
}

func discoveredTargets(pf *wire.Preflight, latency bool) []wire.ThroughputTarget {
	switch {
	case pf == nil:
		return nil
	case latency:
		var targets []wire.ThroughputTarget
		for _, t := range pf.Capabilities.LatencyTargets {
			targets = append(targets, wire.ThroughputTarget(t))
		}
		return targets
	}
	return slices.DeleteFunc(slices.Clone(pf.Capabilities.ThroughputTargets), func(t wire.ThroughputTarget) bool {
		return t.Transport == wire.TransportWebTransportDatagram
	})
}

func (m model) pathChoices(latency bool) []pathChoice {
	pf := m.singleDiscovery()
	if pf == nil {
		return m.sharedPaths(latency)
	}
	resolved := ""
	if c := m.preparedRun.Servers[0].Connection; c != nil {
		if !latency {
			resolved = "→ " + shortOrigin(m.cfg.BaseURL, c.ThroughputTarget.Origin)
		} else if c.LatencyTarget != nil {
			resolved = "→ " + shortOrigin(m.cfg.BaseURL, c.LatencyTarget.Origin)
		}
	}
	choices := []pathChoice{{target: "auto", transport: "auto", label: "Automatic", note: resolved}}
	for _, t := range discoveredTargets(pf, latency) {
		if !slices.ContainsFunc(choices, func(c pathChoice) bool { return c.selects(t.Origin, t.Transport) }) {
			choices = append(choices, pathChoice{
				target:    t.Origin,
				transport: t.Transport,
				label:     connectionSummary(t.Transport, t.Protocol, t.TLS(), latency),
				note:      shortOrigin(m.cfg.BaseURL, t.Origin),
			})
		}
	}
	return choices
}

func (m model) sharedPaths(latency bool) []pathChoice {
	kinds := []string{wire.TransportFetchStream, wire.TransportWebTransport}
	if latency {
		kinds = []string{wire.TransportWebSocket, wire.TransportWebTransport}
	}
	choices := []pathChoice{{target: "auto", transport: "auto", label: "Automatic", note: "each server"}}
	for _, kind := range kinds {
		var unavailable []string
		if m.preparedRun != nil {
			for _, server := range m.preparedRun.Servers {
				offered := func(t wire.ThroughputTarget) bool { return t.Transport == kind }
				if !slices.ContainsFunc(discoveredTargets(discovery(server), latency), offered) {
					unavailable = append(unavailable, server.Server.Name)
				}
			}
		}
		note := "every server"
		if len(unavailable) > 0 {
			note = "unavailable on " + strings.Join(unavailable, ", ")
		}
		label := transportLabel(kind, latency)
		choices = append(choices, pathChoice{target: "auto", transport: kind, label: label, note: note})
	}
	return choices
}

func (m model) selectedThroughputPath() *wire.ThroughputTarget {
	pf := m.singleDiscovery()
	if pf == nil || m.cfg.ThroughputTarget == "auto" || m.cfg.ThroughputTransport == "auto" {
		return nil
	}
	i := slices.IndexFunc(pf.Capabilities.ThroughputTargets, func(t wire.ThroughputTarget) bool {
		return t.Transport == m.cfg.ThroughputTransport && wire.SameOrigin(t.Origin, m.cfg.ThroughputTarget)
	})
	if i < 0 {
		return nil
	}
	return &pf.Capabilities.ThroughputTargets[i]
}

// serverOrigin is the server an entered or pasted address names: its origin, whatever path, query, fragment or
// credentials came with it. An address without a scheme gets HTTPS, or HTTP where TLS is all but unheard of:
// loopback, private and link-local addresses, and names only a local network resolves. A failed HTTPS never falls
// back to HTTP, which would let anyone on the path force a plaintext test.
func serverOrigin(raw string) (string, error) {
	raw = strings.TrimSpace(raw)
	if !strings.Contains(raw, "://") {
		raw = "https://" + raw
		if u, err := url.Parse(raw); err == nil && localHost(u.Hostname()) {
			raw = "http" + strings.TrimPrefix(raw, "https")
		}
	}
	u, err := url.Parse(raw)
	if err == nil && u.Host != "" {
		if origin, err := wire.CatalogOrigin(u.Scheme + "://" + u.Host); err == nil {
			return origin, nil
		}
	}
	return "", errors.New("use the server's address, for example https://meter.example or 192.168.1.20:7246")
}

func localHost(host string) bool {
	host = strings.ToLower(host)
	if ip := net.ParseIP(host); ip != nil {
		return ip.IsLoopback() || ip.IsPrivate() || ip.IsLinkLocalUnicast()
	}
	return !strings.Contains(host, ".") || slices.ContainsFunc([]string{".localhost", ".local", ".lan", ".home.arpa",
		".internal"}, func(suffix string) bool { return strings.HasSuffix(host, suffix) })
}
