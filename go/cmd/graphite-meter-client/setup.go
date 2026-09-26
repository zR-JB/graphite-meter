package main

import (
	"cmp"
	"errors"
	"fmt"
	"net/url"
	"slices"
	"strconv"
	"strings"
	"time"

	"github.com/charmbracelet/bubbles/cursor"
	"github.com/charmbracelet/bubbles/key"
	"github.com/charmbracelet/bubbles/textinput"
	tea "github.com/charmbracelet/bubbletea"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
	"github.com/zR-JB/graphite-meter/go/internal/origin"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

type rowID int

const (
	rowCatalogue rowID = iota
	rowServers
	rowThroughputPath
	rowProtocol
	rowLatencyPath
	rowLatencyServer
	rowLatencyStage
	rowDownloadStage
	rowUploadStage
	rowBidirectionalStage
	rowLoadedLatency
	rowWarmup
	rowLatencyDuration
	rowDownloadDuration
	rowUploadDuration
	rowBidirectionalDuration
	rowCadence
	rowForceStreams
	rowStreams
	rowSkipTLS
	rowReset
)

// The setup mirrors the browser's panels: connection paths, duration and stages, then advanced settings.
var sections = []struct {
	label string
	rows  []rowID
}{
	{"Connection paths", []rowID{rowCatalogue, rowServers, rowThroughputPath, rowProtocol, rowLatencyPath, rowLatencyServer}},
	{"Duration & stages", []rowID{rowLatencyStage, rowDownloadStage, rowUploadStage, rowBidirectionalStage, rowLoadedLatency, rowWarmup, rowLatencyDuration, rowDownloadDuration, rowUploadDuration, rowBidirectionalDuration}},
	{"Advanced", []rowID{rowCadence, rowForceStreams, rowStreams, rowSkipTLS, rowReset}},
}

func (m model) currentRow() rowID { return sections[m.section].rows[m.row] }

// Resolve pointers against the current model copy; never retain them across Update calls.
func stageToggle(cfg *goclient.Config, id rowID) (*bool, string, string) {
	switch id {
	case rowLatencyStage:
		return &cfg.Stages.Latency, "Latency", "idle round trips"
	case rowDownloadStage:
		return &cfg.Stages.Download, "Download", "server to client"
	case rowUploadStage:
		return &cfg.Stages.Upload, "Upload", "client to server, receiver-timed"
	case rowBidirectionalStage:
		return &cfg.Stages.Bidirectional, "Bidirectional", "download and upload at once"
	case rowLoadedLatency:
		return &cfg.LoadedLatency, "Loaded latency", "round trips during transfers"
	}
	return nil, "", ""
}

func durationSetting(cfg *goclient.Config, id rowID) (*time.Duration, string) {
	switch id {
	case rowWarmup:
		return &cfg.Warmup, "Warmup"
	case rowLatencyDuration:
		return &cfg.LatencyDuration, "Latency duration"
	case rowDownloadDuration:
		return &cfg.DownloadDuration, "Download duration"
	case rowUploadDuration:
		return &cfg.UploadDuration, "Upload duration"
	case rowBidirectionalDuration:
		return &cfg.BidirectionalDuration, "Bidirectional duration"
	}
	return nil, ""
}

type cadence struct {
	label    string
	interval time.Duration
}

// Ping cadences share the browser's names; a fixed interval from the command line shows as Custom.
var cadences = []cadence{
	{"Fast (80 ms)", 80 * time.Millisecond},
	{"Medium (250 ms)", 250 * time.Millisecond},
	{"Slow (600 ms)", 600 * time.Millisecond},
}

func cadenceIndex(interval time.Duration) int {
	return slices.IndexFunc(cadences, func(c cadence) bool { return c.interval == interval })
}

func cadenceLabel(interval time.Duration) string {
	if i := cadenceIndex(interval); i >= 0 {
		return cadences[i].label
	}
	return "Custom (" + fmtSetting(interval) + ")"
}

var mechanisms = map[string]string{
	wire.TransportFetchStream:  "Fetch stream",
	wire.TransportWebSocket:    "WebSocket",
	wire.TransportWebTransport: "WebTransport",
}

// shortOrigin names a path's origin by port alone when it shares the catalogue's host.
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

type setupRow struct {
	label, value, note string
	inert              bool // Shown for context; activation explains why it cannot change.
}

func (m model) setupRow(id rowID) setupRow {
	if toggle, label, note := stageToggle(&m.cfg, id); toggle != nil {
		return setupRow{label: label, value: checkbox(*toggle), note: note}
	}
	if value, label := durationSetting(&m.cfg, id); value != nil {
		note := "measured window"
		if id == rowWarmup {
			note = "per stage, before the window opens"
		}
		return setupRow{label: label, value: fmtSetting(*value), note: note}
	}
	switch id {
	case rowCatalogue:
		return setupRow{label: "Catalogue URL", value: m.cfg.BaseURL, note: "server list origin"}
	case rowServers:
		return setupRow{label: "Test servers", value: m.selectedServerNames(), note: m.readinessSummary(), inert: !m.canChooseServers()}
	case rowThroughputPath:
		return m.pathRow("Throughput path", m.cfg.ThroughputTarget, m.cfg.ThroughputTransport, m.throughputPaths())
	case rowProtocol:
		if t := m.selectedThroughputPath(); t != nil && t.Protocol != "negotiated" {
			return setupRow{label: "HTTP version", value: protocolChoiceLabel(t.Protocol), note: "fixed by this path", inert: true}
		}
		return setupRow{label: "HTTP version", value: protocolChoiceLabel(m.cfg.ThroughputProtocol), note: "where the path negotiates"}
	case rowLatencyPath:
		return m.pathRow("Latency path", m.cfg.LatencyTarget, m.cfg.LatencyTransport, m.latencyPaths())
	case rowLatencyServer:
		value := "Automatic"
		if name := m.serverName(m.latencyChoice); m.latencyChoice != "" {
			value = name
		}
		return setupRow{label: "Latency server", value: value, note: "shown first; every server is measured", inert: len(m.readyServers()) < 2}
	case rowCadence:
		return setupRow{label: "Ping cadence", value: cadenceLabel(m.cfg.PingInterval), note: "probe interval"}
	case rowForceStreams:
		return setupRow{label: "Force exact stream count", value: checkbox(m.cfg.TransferStreams.Forced > 0), note: "per server and direction"}
	case rowStreams:
		if m.cfg.TransferStreams.Forced > 0 {
			return setupRow{label: "Streams per server and direction", value: strconv.Itoa(m.cfg.TransferStreams.Forced), note: "1 to 128"}
		}
		return setupRow{label: "Maximum H1 streams per direction", value: strconv.Itoa(m.cfg.TransferStreams.AutomaticMax), note: "HTTP/1.1 paths only"}
	case rowSkipTLS:
		return setupRow{label: "Skip TLS verify", value: checkbox(m.cfg.InsecureSkipTLSVerify), note: "unsafe; refuses sign-in"}
	case rowReset:
		return setupRow{label: "Reset settings", note: "keeps the catalogue and servers"}
	}
	return setupRow{}
}

// activate changes one row. A change to what preparation checked starts a fresh check.
func (m model) activate(id rowID) (tea.Model, tea.Cmd) {
	before := m.cfg
	if toggle, label, _ := stageToggle(&m.cfg, id); toggle != nil {
		*toggle = !*toggle
		m.notice = label + map[bool]string{true: " on.", false: " off."}[*toggle]
		return m.recheckIfPathsChanged(before)
	}
	if value, _ := durationSetting(&m.cfg, id); value != nil {
		m.beginEdit(id, value.String())
		return m, nil
	}
	row := m.setupRow(id)
	switch id {
	case rowCatalogue:
		m.beginEdit(id, m.cfg.BaseURL)
	case rowServers:
		return m.openServerChooser()
	case rowThroughputPath:
		next := nextPath(m.cfg.ThroughputTarget, m.cfg.ThroughputTransport, m.throughputPaths())
		m.cfg.ThroughputTarget, m.cfg.ThroughputTransport = next.target, next.transport
		if t := m.selectedThroughputPath(); t != nil && t.Protocol != "negotiated" {
			m.cfg.ThroughputProtocol = "auto"
		}
		m.notice = "Throughput path: " + next.label + "."
	case rowProtocol:
		if row.inert {
			m.notice = "This path serves " + row.value + " only."
			return m, nil
		}
		m.cfg.ThroughputProtocol = nextChoice(m.cfg.ThroughputProtocol, []string{"auto", "http1", "http2", "http3"})
		m.notice = "HTTP version: " + protocolChoiceLabel(m.cfg.ThroughputProtocol) + "."
	case rowLatencyPath:
		next := nextPath(m.cfg.LatencyTarget, m.cfg.LatencyTransport, m.latencyPaths())
		m.cfg.LatencyTarget, m.cfg.LatencyTransport = next.target, next.transport
		m.notice = "Latency path: " + next.label + "."
	case rowLatencyServer:
		if row.inert {
			m.notice = "Select two ready servers to choose which latency is shown first."
			return m, nil
		}
		m.latencyChoice = nextChoice(m.latencyChoice, append([]string{""}, m.readyServers()...))
		m.notice = "Latency server: " + m.setupRow(id).value + "."
	case rowCadence:
		m.cfg.PingInterval = cadences[(cadenceIndex(m.cfg.PingInterval)+1)%len(cadences)].interval
		m.notice = "Ping cadence: " + cadenceLabel(m.cfg.PingInterval) + "."
	case rowForceStreams:
		if m.cfg.TransferStreams.Forced > 0 {
			m.cfg.TransferStreams.Forced = 0
		} else {
			m.cfg.TransferStreams.Forced = m.cfg.TransferStreams.AutomaticMax
		}
		m.notice = "Stream count: " + m.cfg.TransferStreams.Label("", "") + "."
	case rowStreams:
		m.beginEdit(id, strings.TrimSpace(m.setupRow(id).value))
	case rowSkipTLS:
		m.cfg.InsecureSkipTLSVerify = !m.cfg.InsecureSkipTLSVerify
		m.notice = "TLS verification " + map[bool]string{true: "skipped.", false: "on."}[m.cfg.InsecureSkipTLSVerify]
	case rowReset:
		defaults := goclient.DefaultConfig()
		defaults.BaseURL, defaults.ServerIDs = m.cfg.BaseURL, m.cfg.ServerIDs
		m.cfg = defaults
		m.notice = "Settings reset to defaults."
	}
	return m.recheckIfPathsChanged(before)
}

// preparationInputs is everything a path check depends on; durations and warmup are not.
func preparationInputs(c goclient.Config) string {
	return fmt.Sprint(c.BaseURL, c.ServerIDs, c.ThroughputTarget, c.ThroughputProtocol, c.ThroughputTransport, c.LatencyTarget, c.LatencyTransport,
		c.Stages, c.LoadedLatency, c.PingInterval, c.TransferStreams, c.InsecureSkipTLSVerify)
}

// recheckIfPathsChanged repeats preparation only when the change could alter a prepared path or its credentials.
func (m model) recheckIfPathsChanged(before goclient.Config) (tea.Model, tea.Cmd) {
	if preparationInputs(before) == preparationInputs(m.cfg) {
		return m, nil
	}
	return m.reprepare()
}

type editState struct {
	row   *rowID
	input textinput.Model
	err   string
}

func (m *model) beginEdit(id rowID, value string) {
	in := textinput.New()
	in.Prompt = ""
	in.TextStyle = valueStyle
	in.Cursor.SetMode(cursor.CursorStatic)
	in.SetValue(value)
	in.Focus()
	m.edit = editState{row: &id, input: in}
	m.notice = "Enter applies, esc cancels."
}

func (m model) handleEditKey(msg tea.KeyMsg) (tea.Model, tea.Cmd) {
	switch {
	case key.Matches(msg, keys.abort):
		m.close()
		return m, tea.Quit
	case key.Matches(msg, keys.discard):
		m.edit = editState{}
		m.notice = "Edit canceled."
		return m, nil
	case key.Matches(msg, keys.apply):
		before := m.cfg
		if err := m.commitEdit(); err != nil {
			// The field stays open so the offending text stays editable.
			m.edit.err, m.notice = err.Error(), err.Error()
			return m, nil
		}
		m.edit = editState{}
		return m.recheckIfPathsChanged(before)
	}
	m.edit.err = ""
	var cmd tea.Cmd
	m.edit.input, cmd = m.edit.input.Update(msg)
	return m, cmd
}

func (m *model) commitEdit() error {
	raw := strings.TrimSpace(m.edit.input.Value())
	id := *m.edit.row
	if value, label := durationSetting(&m.cfg, id); value != nil {
		// A bare number is seconds, so "10" works as well as "10s".
		if n, err := strconv.ParseFloat(raw, 64); err == nil {
			raw = fmt.Sprintf("%gs", n)
		}
		d, err := time.ParseDuration(raw)
		switch {
		case err != nil || d < 0:
			return errors.New("use a duration like 800ms, 4s, or 1m; a bare number is seconds")
		case d == 0 && id != rowWarmup:
			return errors.New(label + " must be greater than zero")
		}
		*value = d
		m.notice = label + " " + fmtSetting(d) + "."
		return nil
	}
	switch id {
	case rowCatalogue:
		if !strings.Contains(raw, "://") {
			raw = "http://" + raw
		}
		canonical, err := wire.CanonicalOrigin(strings.TrimSuffix(raw, "/"))
		if err != nil {
			return errors.New("use an http:// or https:// origin, for example https://meter.example")
		}
		if canonical != m.cfg.BaseURL {
			// Catalogue IDs belong to the catalogue that published them.
			m.cfg.ServerIDs, m.latencyChoice = nil, ""
		}
		m.cfg.BaseURL = canonical
		m.notice = "Catalogue " + canonical + "."
	case rowStreams:
		n, err := strconv.Atoi(raw)
		if err != nil || n < 1 || n > 128 {
			return errors.New("streams must be a whole number from 1 to 128")
		}
		if m.cfg.TransferStreams.Forced > 0 {
			m.cfg.TransferStreams.Forced = n
		} else {
			m.cfg.TransferStreams.AutomaticMax = n
		}
		m.notice = "Stream count: " + m.cfg.TransferStreams.Label("http1", wire.TransportFetchStream) + "."
	}
	return nil
}

// pathChoice is one throughput or latency route the setup can cycle through.
type pathChoice struct {
	target    string // The origin the configuration holds, or "auto".
	transport string // The mechanism the configuration holds, or "auto".
	label     string
	note      string
}

func (c pathChoice) selects(target, transport string) bool {
	return origin.Key(c.target) == origin.Key(target) && c.transport == transport
}

func (m model) pathRow(label, target, transport string, choices []pathChoice) setupRow {
	for i, choice := range choices {
		if choice.selects(target, transport) {
			return setupRow{label: label, value: choice.label, note: strings.TrimSpace(fmt.Sprintf("%d/%d  %s", i+1, len(choices), choice.note))}
		}
	}
	// A path from the command line the server does not offer stays visible as asked for.
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

// singleDiscovery is the one checked server's discovery, including one whose paths then failed.
func (m model) singleDiscovery() *wire.Preflight {
	if m.preparedRun == nil || len(m.preparedRun.Servers) != 1 {
		return nil
	}
	server := m.preparedRun.Servers[0]
	if server.Connection != nil {
		return &server.Connection.Preflight
	}
	if failed, ok := errors.AsType[*goclient.PreparationError](server.Err); ok {
		return &failed.Preflight
	}
	return nil
}

func (m model) throughputPaths() []pathChoice {
	pf := m.singleDiscovery()
	if pf == nil {
		return m.sharedPaths(false)
	}
	resolved := ""
	if c := m.preparedRun.Servers[0].Connection; c != nil {
		resolved = "→ " + shortOrigin(m.cfg.BaseURL, c.ThroughputTarget.Origin)
	}
	choices := []pathChoice{{target: "auto", transport: "auto", label: "Automatic", note: resolved}}
	for _, t := range pf.Capabilities.ThroughputTargets {
		if t.Transport == wire.TransportWebTransportDatagram || slices.ContainsFunc(choices, func(c pathChoice) bool { return c.selects(t.Origin, t.Transport) }) {
			continue
		}
		choices = append(choices, pathChoice{target: t.Origin, transport: t.Transport, label: goclient.ConnectionSummary(t.Transport, t.Protocol, t.TLS), note: shortOrigin(m.cfg.BaseURL, t.Origin)})
	}
	return choices
}

func (m model) latencyPaths() []pathChoice {
	pf := m.singleDiscovery()
	if pf == nil {
		return m.sharedPaths(true)
	}
	resolved := ""
	if c := m.preparedRun.Servers[0].Connection; c != nil && c.LatencyTarget != nil {
		resolved = "→ " + shortOrigin(m.cfg.BaseURL, c.LatencyTarget.Origin)
	}
	choices := []pathChoice{{target: "auto", transport: "auto", label: "Automatic", note: resolved}}
	for _, t := range pf.Capabilities.LatencyTargets {
		if slices.ContainsFunc(choices, func(c pathChoice) bool { return c.selects(t.Origin, t.Transport) }) {
			continue
		}
		choices = append(choices, pathChoice{target: t.Origin, transport: t.Transport, label: goclient.ConnectionSummary(t.Transport, t.Protocol, t.TLS), note: shortOrigin(m.cfg.BaseURL, t.Origin)})
	}
	return choices
}

// sharedPaths offers mechanisms rather than origins: each selected server resolves its own origin.
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
				var pf wire.Preflight
				if server.Connection != nil {
					pf = server.Connection.Preflight
				} else if failed, ok := errors.AsType[*goclient.PreparationError](server.Err); ok {
					pf = failed.Preflight
				}
				supported := slices.ContainsFunc(pf.Capabilities.ThroughputTargets, func(t wire.ThroughputTarget) bool { return t.Transport == kind })
				if latency {
					supported = slices.ContainsFunc(pf.Capabilities.LatencyTargets, func(t wire.LatencyTarget) bool { return t.Transport == kind })
				}
				if !supported {
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
