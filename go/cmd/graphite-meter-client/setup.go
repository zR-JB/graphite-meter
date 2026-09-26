package main

import (
	"cmp"
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
	"github.com/zR-JB/graphite-meter/go/internal/origin"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

type setupRow struct {
	label, value, note string
	inert              bool
}

// setting is one setup row: a flag or duration field, or custom view and action functions.
type setting struct {
	label, note string
	flag        func(*goclient.Config) *bool
	span        func(*goclient.Config) *time.Duration
	view        func(model) setupRow
	act         func(*model)
	parse       func(*model, string) error
}

func (s *setting) row(m model) setupRow {
	switch {
	case s.view != nil:
		return s.view(m)
	case s.flag != nil:
		return setupRow{label: s.label, value: m.st.checkbox(*s.flag(&m.cfg)), note: s.note}
	case s.span != nil:
		return setupRow{label: s.label, value: fmtSetting(*s.span(&m.cfg)), note: s.note}
	}
	return setupRow{label: s.label, note: s.note}
}

func toggle(label, note string, field func(*goclient.Config) *bool) *setting {
	return &setting{label: label, note: note, flag: field}
}

func span(label, note string, field func(*goclient.Config) *time.Duration) *setting {
	return &setting{label: label, note: note, span: field}
}

var (
	catalogueRow = &setting{
		view: func(m model) setupRow {
			return setupRow{label: "Catalogue URL", value: m.cfg.BaseURL, note: "server list origin"}
		},
		parse: func(m *model, raw string) error {
			if !strings.Contains(raw, "://") {
				raw = defaultScheme(raw) + raw
			}
			canonical, err := wire.CanonicalOrigin(strings.TrimSuffix(raw, "/"))
			if err != nil {
				return errors.New("use an http:// or https:// origin, for example https://meter.example")
			}
			if canonical != m.cfg.BaseURL {
				m.cfg.ServerIDs, m.latencyChoice = nil, ""
			}
			m.cfg.BaseURL = canonical
			m.notice = "Catalogue " + canonical + "."
			return nil
		},
	}
	serversRow = &setting{view: func(m model) setupRow {
		return setupRow{label: "Test servers", value: m.selectedServerNames(), note: m.readinessSummary(),
			inert: !m.canChooseServers()}
	}}
	throughputPathRow = pathSetting("Throughput path", false, func(c *goclient.Config) (*string, *string) {
		return &c.ThroughputTarget, &c.ThroughputTransport
	})
	latencyPathRow = pathSetting("Latency path", true, func(c *goclient.Config) (*string, *string) {
		return &c.LatencyTarget, &c.LatencyTransport
	})
	protocolRow = &setting{
		view: protocolView,
		act: func(m *model) {
			if row := protocolView(*m); row.inert {
				m.notice = "This path serves " + row.value + " only."
				return
			}
			m.cfg.ThroughputProtocol = nextChoice(m.cfg.ThroughputProtocol, []string{"auto", "http1", "http2", "http3"})
			m.notice = "HTTP version: " + protocolChoiceLabel(m.cfg.ThroughputProtocol) + "."
		},
	}
	latencyServerRow = &setting{
		view: latencyServerView,
		act: func(m *model) {
			if latencyServerView(*m).inert {
				m.notice = "Select two ready servers to choose which latency is shown first."
				return
			}
			m.latencyChoice = nextChoice(m.latencyChoice, append([]string{""}, m.readyServers()...))
			m.notice = "Latency server: " + latencyServerView(*m).value + "."
		},
	}
	warmupRow = span("Warmup", "per stage, before the window opens", func(c *goclient.Config) *time.Duration {
		return &c.Warmup
	})
	cadenceRow = &setting{
		view: func(m model) setupRow {
			return setupRow{label: "Ping cadence", value: cadenceLabel(m.cfg.PingInterval), note: "probe interval"}
		},
		act: func(m *model) {
			m.cfg.PingInterval = cadences[(cadenceIndex(m.cfg.PingInterval)+1)%len(cadences)].interval
			m.notice = "Ping cadence: " + cadenceLabel(m.cfg.PingInterval) + "."
		},
	}
	forceStreamsRow = &setting{
		view: func(m model) setupRow {
			return setupRow{label: "Force exact stream count", value: m.st.checkbox(m.cfg.TransferStreams.Forced > 0),
				note: "per server and direction"}
		},
		act: func(m *model) {
			streams := &m.cfg.TransferStreams
			if streams.Forced > 0 {
				streams.Forced = 0
			} else {
				streams.Forced = streams.AutomaticMax
			}
			m.notice = "Stream count: " + streams.Label("", "") + "."
		},
	}
	streamsRow = &setting{
		view: func(m model) setupRow {
			if n := m.cfg.TransferStreams.Forced; n > 0 {
				return setupRow{label: "Streams per server and direction", value: strconv.Itoa(n), note: streamRange}
			}
			return setupRow{label: "Maximum H1 streams per direction",
				value: strconv.Itoa(m.cfg.TransferStreams.AutomaticMax), note: "HTTP/1.1 paths only"}
		},
		parse: func(m *model, raw string) error {
			n, err := strconv.Atoi(raw)
			if err != nil || n < 1 || n > goclient.MaxTransferStreams {
				return errors.New("streams must be a whole number from " + streamRange)
			}
			if m.cfg.TransferStreams.Forced > 0 {
				m.cfg.TransferStreams.Forced = n
			} else {
				m.cfg.TransferStreams.AutomaticMax = n
			}
			m.notice = "Stream count: " + m.cfg.TransferStreams.Label("http1", wire.TransportFetchStream) + "."
			return nil
		},
	}
	resetRow = &setting{label: "Reset settings", note: "keeps the catalogue and servers", act: func(m *model) {
		defaults := goclient.DefaultConfig()
		defaults.BaseURL, defaults.ServerIDs = m.cfg.BaseURL, m.cfg.ServerIDs
		m.cfg = defaults
		m.notice = "Settings reset to defaults."
	}}
)

var sections = []struct {
	label string
	rows  []*setting
}{
	{"Connection paths", []*setting{catalogueRow, serversRow, throughputPathRow, protocolRow, latencyPathRow,
		latencyServerRow}},
	{"Duration & stages", []*setting{
		toggle("Latency", "idle round trips", func(c *goclient.Config) *bool { return &c.Stages.Latency }),
		toggle("Download", "server to client", func(c *goclient.Config) *bool { return &c.Stages.Download }),
		toggle("Upload", "client to server, receiver-timed", func(c *goclient.Config) *bool {
			return &c.Stages.Upload
		}),
		toggle("Bidirectional", "download and upload at once", func(c *goclient.Config) *bool {
			return &c.Stages.Bidirectional
		}),
		toggle("Loaded latency", "round trips during transfers", func(c *goclient.Config) *bool {
			return &c.LoadedLatency
		}),
		warmupRow,
		span("Latency duration", "measured window", func(c *goclient.Config) *time.Duration {
			return &c.LatencyDuration
		}),
		span("Download duration", "measured window", func(c *goclient.Config) *time.Duration {
			return &c.DownloadDuration
		}),
		span("Upload duration", "measured window", func(c *goclient.Config) *time.Duration {
			return &c.UploadDuration
		}),
		span("Bidirectional duration", "measured window", func(c *goclient.Config) *time.Duration {
			return &c.BidirectionalDuration
		}),
	}},
	{"Advanced", []*setting{cadenceRow, forceStreamsRow, streamsRow,
		toggle("Skip TLS verify", "unsafe; refuses sign-in", func(c *goclient.Config) *bool {
			return &c.InsecureSkipTLSVerify
		}), resetRow}},
}

func (m model) currentRow() *setting { return sections[m.section].rows[m.row] }

func protocolView(m model) setupRow {
	if t := m.selectedThroughputPath(); t != nil && t.Protocol != "negotiated" {
		return setupRow{label: "HTTP version", value: protocolChoiceLabel(t.Protocol), note: "fixed by this path",
			inert: true}
	}
	return setupRow{label: "HTTP version", value: protocolChoiceLabel(m.cfg.ThroughputProtocol),
		note: "where the path negotiates"}
}

func latencyServerView(m model) setupRow {
	value := "Automatic"
	if m.latencyChoice != "" {
		value = m.serverName(m.latencyChoice)
	}
	return setupRow{label: "Latency server", value: value, note: "shown first; every server is measured",
		inert: len(m.readyServers()) < 2}
}

func (m model) activate(s *setting) (tea.Model, tea.Cmd) {
	before := m.cfg
	switch {
	case s == serversRow:
		return m.openServerChooser()
	case s.flag != nil:
		on := s.flag(&m.cfg)
		*on = !*on
		m.notice = s.label + map[bool]string{true: " on.", false: " off."}[*on]
	case s.span != nil:
		m.beginEdit(s, s.span(&m.cfg).String())
	case s.parse != nil:
		m.beginEdit(s, s.row(m).value)
	case s.act != nil:
		s.act(&m)
	}
	return m.recheckIfPathsChanged(before)
}

type cadence struct {
	name     string
	interval time.Duration
}

var streamRange = fmt.Sprintf("1 to %d", goclient.MaxTransferStreams)

var cadences = []cadence{{"Fast", goclient.PingFast}, {"Medium", goclient.PingMedium}, {"Slow", goclient.PingSlow}}

func cadenceIndex(interval time.Duration) int {
	return slices.IndexFunc(cadences, func(c cadence) bool { return c.interval == interval })
}

func cadenceLabel(interval time.Duration) string {
	name := "Custom"
	if i := cadenceIndex(interval); i >= 0 {
		name = cadences[i].name
	}
	return name + " (" + fmtSetting(interval) + ")"
}

var mechanisms = map[string]string{
	wire.TransportFetchStream:  "Fetch stream",
	wire.TransportWebSocket:    "WebSocket",
	wire.TransportWebTransport: "WebTransport",
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

func preparationInputs(c goclient.Config) string {
	return fmt.Sprint(c.BaseURL, c.ServerIDs, c.ThroughputTarget, c.ThroughputProtocol, c.ThroughputTransport,
		c.LatencyTarget, c.LatencyTransport, c.Stages, c.LoadedLatency, c.PingInterval, c.TransferStreams,
		c.InsecureSkipTLSVerify)
}

func (m model) recheckIfPathsChanged(before goclient.Config) (tea.Model, tea.Cmd) {
	if preparationInputs(before) == preparationInputs(m.cfg) {
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
	styles := in.Styles()
	styles.Focused.Text = m.st.value
	in.SetStyles(styles)
	in.SetVirtualCursor(false)
	in.SetValue(value)
	in.Focus()
	m.edit = &editState{row: s, input: in}
	m.notice = "Enter applies, esc cancels."
}

func (m model) handleEditKey(msg tea.KeyPressMsg) (tea.Model, tea.Cmd) {
	switch {
	case key.Matches(msg, keys.discard):
		m.edit = nil
		m.notice = "Edit canceled."
		return m, nil
	case key.Matches(msg, keys.apply):
		before := m.cfg
		if err := m.commitEdit(); err != nil {
			m.edit = &editState{row: m.edit.row, input: m.edit.input, err: err.Error()}
			m.notice = err.Error()
			return m, nil
		}
		m.edit = nil
		return m.recheckIfPathsChanged(before)
	}
	return m.updateEdit(msg)
}

// updateEdit copies the edit state so earlier models keep their own input.
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
	switch {
	case err != nil || d < 0:
		return errors.New("use a duration like 800ms, 4s, or 1m; a bare number is seconds")
	case d == 0 && s != warmupRow:
		return errors.New(s.label + " must be greater than zero")
	}
	*s.span(&m.cfg) = d
	m.notice = s.label + " " + fmtSetting(d) + "."
	return nil
}

func pathSetting(label string, latency bool, field func(*goclient.Config) (*string, *string)) *setting {
	return &setting{
		view: func(m model) setupRow {
			target, transport := field(&m.cfg)
			return m.pathRow(label, *target, *transport, m.pathChoices(latency))
		},
		act: func(m *model) {
			target, transport := field(&m.cfg)
			next := nextPath(*target, *transport, m.pathChoices(latency))
			*target, *transport = next.target, next.transport
			if t := m.selectedThroughputPath(); !latency && t != nil && t.Protocol != "negotiated" {
				m.cfg.ThroughputProtocol = "auto"
			}
			m.notice = label + ": " + next.label + "."
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
	return origin.Key(c.target) == origin.Key(target) && c.transport == transport
}

func (m model) pathRow(label, target, transport string, choices []pathChoice) setupRow {
	for i, choice := range choices {
		if choice.selects(target, transport) {
			return setupRow{
				label: label,
				value: choice.label,
				note:  strings.TrimSpace(fmt.Sprintf("%d/%d  %s", i+1, len(choices), choice.note)),
			}
		}
	}
	mechanism := cmp.Or(mechanisms[transport], transport)
	value := mechanism + " · " + target
	if target == "auto" {
		value = mechanism + " · automatic origin"
	}
	return setupRow{label: label, value: value, note: "not offered by the checked server"}
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
	return choices[(slices.Index(choices, current)+1)%len(choices)]
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

type discoveredPath struct {
	origin, transport, protocol string
	tls                         bool
}

func discoveredPaths(pf *wire.Preflight, latency bool) []discoveredPath {
	if pf == nil {
		return nil
	}
	var paths []discoveredPath
	if latency {
		for _, t := range pf.Capabilities.LatencyTargets {
			paths = append(paths, discoveredPath{t.Origin, t.Transport, t.Protocol, t.TLS})
		}
		return paths
	}
	for _, t := range pf.Capabilities.ThroughputTargets {
		if t.Transport != wire.TransportWebTransportDatagram {
			paths = append(paths, discoveredPath{t.Origin, t.Transport, t.Protocol, t.TLS})
		}
	}
	return paths
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
	for _, p := range discoveredPaths(pf, latency) {
		if !slices.ContainsFunc(choices, func(c pathChoice) bool { return c.selects(p.origin, p.transport) }) {
			choices = append(choices, pathChoice{
				target:    p.origin,
				transport: p.transport,
				label:     goclient.ConnectionSummary(p.transport, p.protocol, p.tls),
				note:      shortOrigin(m.cfg.BaseURL, p.origin),
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
				offered := func(p discoveredPath) bool { return p.transport == kind }
				if !slices.ContainsFunc(discoveredPaths(discovery(server), latency), offered) {
					unavailable = append(unavailable, server.Server.Name)
				}
			}
		}
		note := "every server"
		if len(unavailable) > 0 {
			note = "unavailable on " + strings.Join(unavailable, ", ")
		}
		choices = append(choices, pathChoice{target: "auto", transport: kind, label: mechanisms[kind], note: note})
	}
	return choices
}

func (m model) selectedThroughputPath() *wire.ThroughputTarget {
	pf := m.singleDiscovery()
	if pf == nil || m.cfg.ThroughputTarget == "auto" || m.cfg.ThroughputTransport == "auto" {
		return nil
	}
	i := slices.IndexFunc(pf.Capabilities.ThroughputTargets, func(t wire.ThroughputTarget) bool {
		return t.Transport == m.cfg.ThroughputTransport && origin.Key(t.Origin) == origin.Key(m.cfg.ThroughputTarget)
	})
	if i < 0 {
		return nil
	}
	return &pf.Capabilities.ThroughputTargets[i]
}

// defaultScheme assumes HTTPS for a bare host, except a loopback server under local development.
func defaultScheme(raw string) string {
	u, err := url.Parse("//" + raw)
	if err != nil {
		return "https://"
	}
	ip := net.ParseIP(u.Hostname())
	if strings.EqualFold(u.Hostname(), "localhost") || ip != nil && ip.IsLoopback() {
		return "http://"
	}
	return "https://"
}
