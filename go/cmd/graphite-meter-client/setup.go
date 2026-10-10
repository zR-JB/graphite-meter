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

type setupRow struct {
	label, value, help string
	placeholder        string // Shown greyed while the value is empty, and in its editor.
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
		on := *s.flag(&m.cfg)
		return setupRow{label: s.label, value: m.st.checkbox(on) + " " + fmtSetting(*s.span(&m.cfg)), help: s.help,
			inert: !on}
	case s.flag != nil:
		return setupRow{label: s.label, value: m.st.checkbox(*s.flag(&m.cfg)), help: s.help}
	case s.span != nil:
		return setupRow{label: s.label, value: fmtSetting(*s.span(&m.cfg)), help: s.help}
	}
	return setupRow{label: s.label, help: s.help}
}

func stageSetting(label, what string, flag func(*goclient.Config) *bool,
	span func(*goclient.Config) *time.Duration) *setting {
	bound := goclient.StageBound
	return &setting{label: label, flag: flag, span: span, help: fmt.Sprintf(
		"%s. ←/→ step it (%s–%s; a server may allow less), space on/off.",
		what, fmtSetting(bound.Min), fmtSetting(bound.Max))}
}

var (
	startRow    = &setting{label: "Start test", help: "Runs the checked stages in order. r starts from any row."}
	advancedRow = &setting{
		view: func(m model) setupRow {
			value := map[bool]string{false: "▸ hidden", true: "▾ shown"}[m.advanced]
			return setupRow{label: "Advanced", value: value,
				help: "Warmup, probe cadence, streams and TLS. ←/→ shows or hides."}
		},
		cycle: func(m *model, _ int) { m.advanced = !m.advanced },
	}
	catalogueRow = &setting{
		view: func(m model) setupRow {
			return setupRow{label: "Server address", value: m.cfg.BaseURL, placeholder: "type the server's address",
				help: "The Graphite Meter server to test; it lists its test servers. enter types its address."}
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
			m.notice = "Server " + canonical + "."
			return nil
		},
	}
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
			if row := protocolView(*m); row.inert {
				m.notice = "This path serves " + row.value + " only."
				return
			}
			protocols := []string{"auto", "http1", "http2", "http3"}
			m.cfg.ThroughputProtocol = nextChoice(m.cfg.ThroughputProtocol, protocols, step)
			m.notice = "HTTP version: " + protocolLabel(m.cfg.ThroughputProtocol) + "."
		},
	}
	warmupRow = &setting{label: "Warmup", span: func(c *goclient.Config) *time.Duration { return &c.Warmup },
		help: fmt.Sprintf("Ramp-up before each window, at least ten round trips. ←/→ ±100 ms (%s–%s).",
			fmtSetting(goclient.WarmupBound.Min), fmtSetting(goclient.WarmupBound.Max))}
	idleCadenceRow = cadenceSetting("Idle latency cadence", "when idle", func(c *goclient.Config) *time.Duration {
		return &c.PingInterval
	})
	loadedCadenceRow = cadenceSetting("Loaded latency cadence", "during transfers",
		func(c *goclient.Config) *time.Duration { return &c.LoadedPingInterval })
	forceStreamsRow = &setting{
		view: func(m model) setupRow {
			return setupRow{label: "Force exact stream count", value: m.st.checkbox(m.cfg.TransferStreams.Forced > 0),
				help: "Off: each path picks its count. On: the count below everywhere."}
		},
		cycle: func(m *model, _ int) {
			streams := &m.cfg.TransferStreams
			if streams.Forced > 0 {
				streams.Forced = 0
			} else {
				streams.Forced = streams.AutomaticMax
			}
			m.notice = "Stream count: " + streamsLabel(*streams, "", "") + "."
		},
	}
	streamsRow = &setting{
		view: func(m model) setupRow {
			row := setupRow{label: "Maximum H1 streams per direction", value: strconv.Itoa(*streamCount(&m.cfg)),
				help: fmt.Sprintf("Upper bound on HTTP/1.1 paths. ←/→ ±1 (1–%d).", goclient.MaxStreams)}
			if m.cfg.TransferStreams.Forced > 0 {
				row.label = "Streams per server and direction"
				row.help = fmt.Sprintf("Exact streams per server and direction. ←/→ ±1 (1–%d).", goclient.MaxStreams)
			}
			return row
		},
		cycle: func(m *model, step int) {
			n := streamCount(&m.cfg)
			*n = min(max(*n+step, 1), goclient.MaxStreams)
			m.notice = "Stream count: " + streamsLabel(m.cfg.TransferStreams, "http1", wire.TransportFetchStream) + "."
		},
		parse: func(m *model, raw string) error {
			n, err := strconv.Atoi(raw)
			if err != nil || n < 1 || n > goclient.MaxStreams {
				return fmt.Errorf("streams must be a whole number from 1 to %d", goclient.MaxStreams)
			}
			*streamCount(&m.cfg) = n
			m.notice = "Stream count: " + streamsLabel(m.cfg.TransferStreams, "http1", wire.TransportFetchStream) + "."
			return nil
		},
	}
	resetRow = &setting{label: "Reset settings", help: "Restores defaults; keeps the catalogue and servers.",
		act: func(m *model) {
			if !m.resetPrompt {
				m.resetPrompt = true
				m.notice = "Press enter again to reset every setting; any other key keeps them."
				return
			}
			defaults := goclient.DefaultConfig()
			defaults.BaseURL, defaults.ServerIDs = m.cfg.BaseURL, m.cfg.ServerIDs
			m.cfg, m.resetPrompt = defaults, false
			m.notice = "Settings reset to defaults."
		}}
)

func streamCount(c *goclient.Config) *int {
	if c.TransferStreams.Forced > 0 {
		return &c.TransferStreams.Forced
	}
	return &c.TransferStreams.AutomaticMax
}

var setupGroups = []struct {
	label string
	rows  []*setting
}{
	{"", []*setting{startRow}},
	{"Connection", []*setting{catalogueRow, serversRow, throughputPathRow, protocolRow, latencyPathRow}},
	{"Stages", []*setting{
		stageSetting("Latency", "Idle round trips",
			func(c *goclient.Config) *bool { return &c.Stages.Latency },
			func(c *goclient.Config) *time.Duration { return &c.LatencyDuration }),
		stageSetting("Download", "Server to client",
			func(c *goclient.Config) *bool { return &c.Stages.Download },
			func(c *goclient.Config) *time.Duration { return &c.DownloadDuration }),
		stageSetting("Upload", "Client to server, receiver-timed",
			func(c *goclient.Config) *bool { return &c.Stages.Upload },
			func(c *goclient.Config) *time.Duration { return &c.UploadDuration }),
		stageSetting("Bidirectional", "Download and upload at once",
			func(c *goclient.Config) *bool { return &c.Stages.Bidirectional },
			func(c *goclient.Config) *time.Duration { return &c.BidirectionalDuration }),
		{label: "Loaded latency", flag: func(c *goclient.Config) *bool { return &c.LoadedLatency },
			help: "Round trips during transfers: the latency load adds. space on/off."},
	}},
	{"", []*setting{advancedRow, warmupRow, idleCadenceRow, loadedCadenceRow, forceStreamsRow, streamsRow,
		{label: "Skip TLS verify", flag: func(c *goclient.Config) *bool { return &c.InsecureSkipTLSVerify },
			help: "Accepts any certificate. Unsafe; sign-in is refused. space on/off."},
		resetRow}},
}

func (m model) rows() []*setting {
	var rows []*setting
	for _, g := range setupGroups {
		for _, s := range g.rows {
			rows = append(rows, s)
			if s == advancedRow && !m.advanced {
				break
			}
		}
	}
	return rows
}

func (m model) currentRow() *setting { return m.rows()[m.row] }

func protocolView(m model) setupRow {
	if t := m.selectedThroughputPath(); t != nil && t.Protocol != "negotiated" {
		return setupRow{label: "HTTP version", value: protocolLabel(t.Protocol), inert: true,
			help: "Fixed by this path; pick another path to change it."}
	}
	return setupRow{label: "HTTP version", value: protocolLabel(m.cfg.ThroughputProtocol),
		help: "Where the path negotiates. ←/→ Automatic, HTTP/1.1, HTTP/2, HTTP/3."}
}

func (m model) activate(s *setting) (tea.Model, tea.Cmd) {
	before := m.cfg
	switch {
	case s == startRow:
		return m.startRun()
	case s == serversRow:
		return m.openServerChooser()
	case s.act != nil:
		s.act(&m)
	case s.span != nil:
		m.beginEdit(s, s.span(&m.cfg).String())
	case s.parse != nil:
		m.beginEdit(s, s.row(m).value)
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
		m.notice = s.label + " " + fmtSetting(*d) + "."
	case s.flag != nil:
		m.setFlag(s, step > 0)
	}
	return m.recheckIfPathsChanged(before)
}

func (m *model) setFlag(s *setting, on bool) {
	*s.flag(&m.cfg) = on
	m.notice = s.label + map[bool]string{true: " on.", false: " off."}[on]
}

func enterVerb(s *setting) string {
	switch {
	case s == serversRow || s == advancedRow:
		return "open"
	case s == resetRow:
		return "reset"
	case s.span != nil || s.parse != nil:
		return "edit"
	case s.flag != nil:
		return "on/off"
	}
	return "next"
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
	styles := in.Styles()
	styles.Focused.Text, styles.Focused.Placeholder = m.st.value, m.st.muted
	styles.Cursor.Blink = false
	in.SetStyles(styles)
	in.SetValue(value)
	in.Placeholder = s.row(*m).placeholder
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
		return errors.New("use a duration like 800ms, 4s, or 1m; a bare number is seconds")
	}
	if err := s.inBounds(d); err != nil {
		return err
	}
	*s.span(&m.cfg) = d
	m.notice = s.label + " " + fmtSetting(d) + "."
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

func cadenceSetting(label, when string, field func(*goclient.Config) *time.Duration) *setting {
	var spacings []string
	for _, c := range cadences[1:] {
		spacings = append(spacings, strconv.FormatInt(c.interval.Milliseconds(), 10))
	}
	help := "Probe spacing " + when + ". ←/→ reply-driven, " + strings.Join(spacings, ", ") + " ms."
	return &setting{
		view: func(m model) setupRow {
			return setupRow{label: label, value: cadenceLabel(*field(&m.cfg)), help: help}
		},
		cycle: func(m *model, step int) {
			interval := field(&m.cfg)
			*interval = cadences[(cadenceIndex(*interval)+step+len(cadences))%len(cadences)].interval
			m.notice = label + ": " + cadenceLabel(*interval) + "."
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
	return (c.target == target || wire.SameOrigin(c.target, target)) && c.transport == transport
}

func (m model) pathRow(label, target, transport string, latency bool) setupRow {
	choices := m.pathChoices(latency)
	carries := map[bool]string{false: "How transfers reach the server", true: "How probes travel"}[latency]
	help := fmt.Sprintf("%s. ←/→ picks one of %d.", carries, len(choices))
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
	return setupRow{label: label, value: value, help: "Not offered by the checked server. " + help}
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
