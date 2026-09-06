package main

import (
	"cmp"
	"fmt"
	"slices"
	"strings"

	"github.com/charmbracelet/bubbles/help"
	"github.com/charmbracelet/lipgloss"
	"github.com/muesli/termenv"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// shellMargin is shellStyle's horizontal margin.
const shellMargin = 2

func (m model) View() string {
	w := m.innerWidth()

	var b strings.Builder
	b.WriteString(m.header(w))
	b.WriteString("\n\n")
	if m.serverDetailsOpen {
		b.WriteString(m.serverDetailsOverlay(w))
	} else if m.serverChooser {
		b.WriteString(m.serverChooserView(w))
	} else if m.mode == modeRun {
		b.WriteString(m.runView(w))
	} else {
		b.WriteString(m.configView(w))
	}
	b.WriteString("\n")
	b.WriteString(m.helpView())
	return shellStyle.Render(b.String())
}

func (m model) innerWidth() int {
	return max(m.width-4, 40)
}

func fitLine(s string, w int) string {
	if lipgloss.Width(s) <= w {
		return s
	}
	return lipgloss.NewStyle().MaxWidth(w).Render(s)
}

func fitBlock(s string, w int) string {
	var b strings.Builder
	changed := false
	rest := s
	for {
		line, tail, more := strings.Cut(rest, "\n")
		fitted := fitLine(line, w)
		if fitted != line && !changed {
			b.Grow(len(s))
			b.WriteString(s[:len(s)-len(rest)]) // the verbatim prefix up to this line
			changed = true
		}
		if changed {
			b.WriteString(fitted)
			if more {
				b.WriteByte('\n')
			}
		}
		if !more {
			break
		}
		rest = tail
	}
	if !changed {
		return s
	}
	return b.String()
}

func (m model) header(w int) string {
	status := m.prepareStatus
	if m.mode == modeRun {
		status = emptyDash(m.status)
	}
	left := titleStyle.Render("Graphite Meter")
	right := pillStyle.Render(status)
	spacer := strings.Repeat(" ", max(1, w-lipgloss.Width(left)-lipgloss.Width(right)))
	line := fitLine(left+spacer+right, w)

	target := m.cfg.BaseURL
	if m.server != "" {
		target = m.server
	}
	return line + "\n" + fitLine(mutedStyle.Render("native go client "+goclient.Version)+"  "+accentStyle.Render(target), w)
}

const (
	// panelBorderWidth is the column pair a panel's rounded border draws outside its lipgloss width.
	panelBorderWidth = 2
	// gutterWidth separates two side-by-side panels.
	gutterWidth  = 2
	twoColumnMin = 115
)

func splitColumns(w int) (leftW, rightW int, twoCol bool) {
	if w < twoColumnMin {
		return w - panelBorderWidth, w - panelBorderWidth, false
	}
	inner := w - gutterWidth - 2*panelBorderWidth
	leftW = inner * 12 / 20
	return leftW, inner - leftW, true
}

func (m model) configView(w int) string {
	var b strings.Builder
	b.WriteString(m.tabBar(w))
	b.WriteString("\n\n")
	// An outstanding approval is what the screen is waiting on, so it goes above the sections it is blocking.
	if auth := m.authView(); auth != nil {
		b.WriteString(panelStyle.Width(w - 2).Render(fitBlock(strings.Join(auth, "\n"), w-6)))
		b.WriteString("\n\n")
	}

	leftW, rightW, twoCol := splitColumns(w)
	menu := panelStyle.Width(leftW).Render(fitBlock(m.sectionView(leftW-4), leftW-4))
	summary := panelStyle.Width(rightW).Render(fitBlock(m.planView(), rightW-4))
	if twoCol {
		b.WriteString(lipgloss.JoinHorizontal(lipgloss.Top, menu, "  ", summary))
	} else {
		b.WriteString(menu)
		b.WriteString("\n\n")
		b.WriteString(summary)
	}
	if m.notice != "" {
		b.WriteString("\n\n")
		b.WriteString(fitLine(mutedStyle.Render(m.notice), w))
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
	// Clipping the trailing tabs holds the shell block at w. A longer line pads every other line past the terminal.
	return fitLine(line, w)
}

func (m model) sectionView(w int) string {
	switch m.section {
	case sectionServers:
		return m.serversView(w)
	case sectionRunSetup:
		return m.stagesView(w)
	case sectionTiming:
		return m.timingView(w)
	case sectionConnections:
		return m.networkView(w)
	case sectionRun:
		return m.runMenuView(w)
	default:
		return ""
	}
}

const serverURLColumn = 21

func (m model) serversView(w int) string {
	lines := []string{accentStyle.Render("Server")}
	if m.canChooseServers() {
		lines = append(lines, mutedStyle.Render("Press s to choose servers for this test"))
	}
	active := activePreset(m.cfg.BaseURL)
	row := func(selected bool, name, url, note string) string {
		return strings.TrimRight(fmt.Sprintf("%s %-10s %s  %s", checkbox(selected), name, url, note), " ")
	}
	for i, preset := range serverPresets {
		line := row(i == active, preset.name, valueStyle.Render(pad(preset.url, serverURLColumn)), mutedStyle.Render(preset.note))
		lines = append(lines, m.menuLine(i, line, w))
	}
	url, note := valueStyle.Render(pad(m.cfg.BaseURL, serverURLColumn)), ""
	if m.edit.kind == editURL {
		url = m.edit.input.View()
		note = mutedStyle.Render("enter applies · esc cancels") + m.editError()
	}
	lines = append(lines,
		m.menuLine(len(serverPresets), row(active == -1, "Custom URL", url, note), w),
		"",
		mutedStyle.Render("No port → :80 for http, :443 for https."),
	)
	return lipgloss.JoinVertical(lipgloss.Left, lines...)
}

func (m model) stagesView(w int) string {
	var rows []string
	for _, setting := range stageSettings(&m.cfg) {
		rows = append(rows, toggleLine(setting.label, *setting.value, setting.note))
	}
	return m.listWithTitle("Stage Profile", rows, w)
}

func (m model) timingView(w int) string {
	var rows []string
	for i, setting := range timingSettings(&m.cfg) {
		row := valueLine(setting.label, setting.value.String(), setting.note)
		if m.edit.kind == editDuration && i == m.row {
			row = valueLine(setting.label, m.edit.input.View(), "editing") + m.editError()
		}
		rows = append(rows, row)
	}
	return m.listWithTitle("Timing", rows, w)
}

func (m model) networkView(w int) string {
	autoMax := valueLine("Auto H1 max", fmt.Sprintf("%d", m.cfg.TransferStreams.AutomaticMax), "per direction")
	if m.cfg.ThroughputTransport == wire.TransportWebTransport {
		autoMax = inertValueLine("Auto H1 max", fmt.Sprintf("%d", m.cfg.TransferStreams.AutomaticMax), "unused over WebTransport")
	}
	rows := make([]string, rowConnectionsCount)
	rows[rowThroughputPath] = pathRow("Throughput path", m.cfg.ThroughputTarget, m.cfg.ThroughputTransport, m.throughputPaths())
	rows[rowThroughputProtocol] = m.throughputProtocolRow()
	rows[rowLatencyPath] = pathRow("Latency path", m.cfg.LatencyTarget, m.cfg.LatencyTransport, m.latencyPaths())
	rows[rowAutoStreams] = autoMax
	// The value already spells out what automatic resolves to, so the note only has to say what to type to get it back.
	rows[rowStreams] = valueLine("Streams", m.cfg.TransferStreams.Label(m.cfg.ThroughputProtocol, m.cfg.ThroughputTransport), "0 = auto")
	rows[rowSkipTLS] = toggleLine("Skip TLS verify", m.cfg.InsecureSkipTLSVerify, "unsafe")
	rows[rowReset] = warnStyle.Render("Reset to defaults")
	if m.edit.field == "auto-streams" {
		rows[rowAutoStreams] = valueLine("Auto H1 max", m.edit.input.View(), "editing") + m.editError()
	}
	if m.edit.field == "streams" {
		rows[rowStreams] = valueLine("Streams", m.edit.input.View(), "editing") + m.editError()
	}
	return m.listWithTitle("Connections", rows, w)
}

func (m model) throughputProtocolRow() string {
	if t := m.selectedThroughputPath(); !m.multipleServers() && t != nil && t.Protocol != protocolNegotiated {
		return inertValueLine("HTTP version", protocolChoiceLabel(t.Protocol), "fixed by this path")
	}
	return valueLine("HTTP version", protocolChoiceLabel(m.cfg.ThroughputProtocol), "where the path negotiates")
}

func (m model) runMenuView(w int) string {
	label := "Start measurement · " + m.prepareStatus
	switch m.prepareStatus {
	case "ready":
		label = accentStyle.Render(label)
	case "failed":
		label = warnStyle.Render(label)
	case statusIdle:
		label = mutedStyle.Render(label)
	default:
		label = m.spin.View() + " " + label
	}
	return m.listWithTitle("Start", []string{label}, w)
}

// editError trails the edited field with the reason its last commit fails, so the answer sits where the eye already is.
func (m model) editError() string {
	if m.edit.err == "" {
		return ""
	}
	return "  " + errorStyle.Render(m.edit.err)
}

func (m model) listWithTitle(title string, rows []string, w int) string {
	lines := []string{accentStyle.Render(title)}
	for i, row := range rows {
		lines = append(lines, m.menuLine(i, row, w))
	}
	return lipgloss.JoinVertical(lipgloss.Left, lines...)
}

func (m model) menuLine(i int, s string, w int) string {
	if i != m.row {
		return "  " + s
	}
	return "› " + selectedStyle.Width(max(12, w-2)).Render(fitLine(s, w-2))
}

func (m model) planView() string {
	pending := "Checking"
	if m.prepareStatus == statusIdle {
		pending = "Not checked"
	}
	throughput, latency, observed := pending, pending, ""
	value := valueStyle
	if p := m.prepared; p != nil {
		throughput, latency, observed = p.ThroughputSummary(), p.LatencySummary(), p.Probe.ProtocolNegotiated
		if m.preparedRun != nil && !m.preparedRun.FreshFor(m.cfg) || m.preparedRun == nil && !p.FreshFor(m.cfg) {
			value = mutedStyle
		}
	}
	lines := []string{accentStyle.Render("Connection readiness")}
	lines = append(lines, m.checklistView()...)
	if m.prepareError != "" {
		lines = append(lines, warnStyle.Render(m.prepareError), mutedStyle.Render(m.preparationHelp()))
	}
	lines = append(lines,
		"",
		field("Throughput", value.Render(throughput)),
		field("Latency", value.Render(latency)),
		field("Observed", value.Render(goclient.ProtocolLabel(observed))),
		"",
		mutedStyle.Render("Run order"),
	)
	for _, line := range runOrder(m.cfg) {
		lines = append(lines, "  "+line)
	}
	return lipgloss.JoinVertical(lipgloss.Left, lines...)
}

func (m model) checklistView() []string {
	lines := make([]string, 0, 4)
	for _, c := range m.connectionChecks() {
		line := m.checkGlyph(c.state) + " " + c.label
		if c.note != "" {
			line += mutedStyle.Render("  " + c.note)
		}
		lines = append(lines, line)
	}
	return lines
}

func (m model) checkGlyph(state checkState) string {
	switch state {
	case checkActive:
		return m.spin.View()
	case checkDone:
		return successStyle.Render("✓")
	case checkFailed:
		return errorStyle.Render("✗")
	case checkSkipped:
		return mutedStyle.Render("·")
	default:
		return mutedStyle.Render("○")
	}
}

func (m model) authView() []string {
	if m.auth == nil {
		return nil
	}
	waited := m.now.Sub(m.authSince)
	prompt := "Press enter to open the approval page in your browser"
	if m.authOpened {
		prompt = "Approve this client in the browser"
	}
	code := lipgloss.JoinHorizontal(lipgloss.Center,
		labelStyle.Render("Match this code")+" ",
		codeStyle.Render(m.auth.Code),
		mutedStyle.Render("  waiting "+fmtClock(waited)+" · expires in "+fmtClock(goclient.AuthorizationTimeout-waited)),
	)
	return []string{
		m.spin.View() + " " + accentStyle.Render(prompt),
		"",
		code,
		mutedStyle.Render(m.auth.BrowserURL),
	}
}

func (m model) runView(w int) string {
	leftW, rightW, twoCol := splitColumns(w)
	summary := panelStyle.Width(leftW).Render(fitBlock(m.summaryView(leftW-4), leftW-4))
	live := panelStyle.Width(rightW).Render(fitBlock(m.liveView(rightW-4), rightW-4))
	var b strings.Builder
	if twoCol {
		b.WriteString(lipgloss.JoinHorizontal(lipgloss.Top, summary, "  ", live))
	} else {
		b.WriteString(summary)
		b.WriteString("\n\n")
		b.WriteString(live)
	}
	if len(m.visibleResults()) > 0 {
		b.WriteString("\n\n")
		b.WriteString(panelStyle.Width(w - 2).Render(fitBlock(m.resultsView(w-6), w-6)))
	}
	if m.hasServerBreakdown() {
		b.WriteString("\n\n")
		b.WriteString(fitLine(m.serverResultNotice()+" · d Details", w))
	}
	if m.err != nil {
		b.WriteString("\n\n")
		b.WriteString(fitLine(errorStyle.Render(m.err.Error()), w))
	}
	return b.String()
}

func (m model) summaryView(w int) string {
	server := m.server
	server = cmp.Or(server, "probing "+m.cfg.BaseURL)
	throughput := m.runPath(m.throughputTransport, m.throughputProtocol, m.target)
	latency := m.runPath(m.latencyTransport, m.latencyProtocol, m.latencyTarget)
	streams := m.cfg.TransferStreams.Label(m.throughputProtocol, m.throughputTransport)
	if m.hasServerBreakdown() {
		names := make([]string, 0, len(m.runDetails.Selection))
		paths := []string{}
		for _, participant := range m.runDetails.Servers {
			names = append(names, participant.Server.Name)
			path := participant.Throughput.Transport + " / " + participant.Throughput.Protocol
			if !slices.Contains(paths, path) {
				paths = append(paths, path)
			}
			if participant.Server.ID == m.latencyFocus && participant.LatencyTarget != nil {
				latency = participant.Server.Name + " · " + participant.LatencyTarget.Transport + " · " + participant.LatencyTarget.Origin
			}
		}
		server = strings.Join(names, ", ")
		throughput = strings.Join(paths, " + ")
		if len(names) > 1 {
			throughput = "Combined · " + throughput
			streams = "Automatic per server"
			if m.cfg.TransferStreams.Forced > 0 {
				streams = fmt.Sprintf("%d per server / direction", m.cfg.TransferStreams.Forced)
			}
		}
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
		field("Target", valueStyle.Render(server)),
		field("Stage", mark+valueStyle.Render(emptyDash(m.stage))+mutedStyle.Render(" / "+emptyDash(m.status))),
		field("Profile", valueStyle.Render(stageSummary(m.cfg.Stages))),
		field("Throughput", throughput),
		field("Latency", latency),
		field("Streams", valueStyle.Render(streams)+mutedStyle.Render("  warmup "+m.cfg.Warmup.String()+"  ping "+m.cfg.PingInterval.String())),
		"",
	}
	lines = append(lines, m.timelineView(w)...)
	if m.complete {
		lines = append(lines, "", successStyle.Render("✓")+accentStyle.Render(" Finished. Press esc for setup or r to run again."))
	}
	return lipgloss.JoinVertical(lipgloss.Left, lines...)
}

func (m model) runPath(transport, protocol, target string) string {
	if target == "" {
		return mutedStyle.Render("--")
	}
	summary := goclient.ConnectionSummary(transport, protocol, strings.HasPrefix(target, "https://"))
	return valueStyle.Render(summary) + mutedStyle.Render("  "+shortOrigin(m.cfg.BaseURL, target))
}

func (m model) timelineView(w int) []string {
	if len(m.stages) == 0 {
		return nil
	}
	lines := []string{mutedStyle.Render("Stages")}
	barW := clamp(w-38, 8, 40)
	for _, s := range m.stages {
		name := labelStyle.Render(fmt.Sprintf("%-14s", s.name))
		switch s.state {
		case stagePreparing:
			lines = append(lines, name+accentStyle.Render("◐ ")+m.spin.View()+mutedStyle.Render(" preparing transports"))
		case stageWarmup:
			lines = append(lines, name+accentStyle.Render("◐ ")+m.spin.View()+
				mutedStyle.Render(" warmup ")+valueStyle.Render(fmtClock(m.now.Sub(s.since))))
		case stageMeasuring:
			elapsed := m.now.Sub(s.since)
			progress := m.spin.View() + mutedStyle.Render(" measuring")
			if s.duration > 0 {
				progress = renderBar(elapsed.Seconds(), s.duration.Seconds(), barW, false) +
					"  " + valueStyle.Render(fmtClock(elapsed)) + mutedStyle.Render(" / "+s.duration.String())
			}
			lines = append(lines, name+accentStyle.Render("● ")+progress)
		case stageDone:
			lines = append(lines, name+successStyle.Render("✓ ")+mutedStyle.Render(s.duration.String()))
		case stageStopped:
			lines = append(lines, name+errorStyle.Render("✗ ")+mutedStyle.Render("stopped"))
		default:
			lines = append(lines, name+mutedStyle.Render("○ pending"))
		}
	}
	return lines
}

func (m model) liveView(w int) string {
	scale := m.rateScale()
	lines := []string{
		accentStyle.Render("Live Telemetry"),
		m.liveRateLine("download", goclient.Down, scale, w),
		m.liveRateLine("upload  ", goclient.Up, scale, w),
		latencyLine(m.latency, m.lostStreak),
	}
	if name := m.latencyServerName(); name != "" {
		lines = append(lines, mutedStyle.Render("Latency to "+name+" · l switches server"))
	}
	return lipgloss.JoinVertical(lipgloss.Left, lines...)
}

func (m model) rateScale() float64 {
	return max(m.peaks[goclient.Down], m.peaks[goclient.Up])
}

func (m model) resultsView(w int) string {
	lines := []string{accentStyle.Render("Results")}

	type row struct{ head, tail string }
	var scale float64
	var bars []row
	fixed := 0
	for _, r := range m.visibleResults() {
		if isLatencyResult(r) {
			continue
		}
		scale = max(scale, r.PeakBps, r.MeanBps)
		dir := "down"
		if r.Direction == goclient.Up {
			dir = "up"
		}
		peak := "--"
		if r.PeakBps > 0 {
			peak = fmtRate(r.PeakBps)
		}
		note := "peak " + peak + "  " + fmtBytes(r.TotalBytes)
		if r.ServerAuth {
			note += "  server-clock"
		}
		rate := fmtRate(r.MeanBps)
		if r.Unavailable {
			rate = "--"
		}
		b := row{
			head: mutedStyle.Render(pad(r.Stage, 13)) + " " + accentStyle.Render(pad(dir, 4)) + " ",
			tail: "  " + valueStyle.Render(fmt.Sprintf("%13s", rate)) + "  " + mutedStyle.Render(note),
		}
		bars = append(bars, b)
		fixed = max(fixed, lipgloss.Width(b.head)+lipgloss.Width(b.tail))
	}
	barW := clamp(w-fixed, 8, 48)

	next := 0
	for _, r := range m.visibleResults() {
		if isLatencyResult(r) {
			lines = append(lines, fmt.Sprintf("%s %s   p50 %s  p95 %s  %s",
				mutedStyle.Render(pad(r.Stage, 13)),
				labelStyle.Render("latency"),
				valueStyle.Render(fmtMs(r.Latency.P50)),
				valueStyle.Render(fmtMs(r.Latency.P95)),
				mutedStyle.Render(latencyOutcomeSummary(r.Latency)),
			))
			if diagnostic := reflectorTimingSummary(r.Latency.ReflectorTiming); diagnostic != "" {
				lines = append(lines, mutedStyle.Width(max(1, w-2)).MarginLeft(2).Render(diagnostic))
			}
			if r.Err != nil {
				lines = append(lines, errorStyle.Render("  Incomplete: "+r.Err.Error()))
			}
			continue
		}
		b := bars[next]
		next++
		lines = append(lines, b.head+renderBar(r.MeanBps, scale, barW, false)+b.tail)
		if r.Err != nil {
			lines = append(lines, errorStyle.Render("  Incomplete: "+r.Err.Error()))
		}
	}
	return lipgloss.JoinVertical(lipgloss.Left, lines...)
}

func (m model) finalReport() string {
	if len(m.visibleResults()) == 0 || !m.complete && m.err == nil {
		return ""
	}
	lipgloss.SetColorProfile(termenv.Ascii)
	return m.resultsView(m.innerWidth()) + "\n\n" + m.serverResultsView(m.innerWidth())
}

// isLatencyResult includes directionless timeout-only and unresolved-only latency summaries.
func isLatencyResult(r goclient.Result) bool {
	return r.Direction == ""
}

// helpView is the footer. The model is the key map it renders, so the listing follows whichever screen is on show.
func (m model) helpView() string {
	m.help.Width = m.innerWidth()
	return fitBlock(m.help.View(m), m.innerWidth())
}

// newHelp is the footer renderer, dressed in this program's styles rather than the bubble's defaults.
func newHelp() help.Model {
	h := help.New()
	h.Styles.ShortKey, h.Styles.FullKey = labelStyle, labelStyle
	h.Styles.ShortDesc, h.Styles.FullDesc = mutedStyle, mutedStyle
	h.Styles.ShortSeparator, h.Styles.FullSeparator = subtleRuleStyle, subtleRuleStyle
	h.Styles.Ellipsis = subtleRuleStyle
	return h
}

func (m model) liveRateLine(label string, dir goclient.Direction, scale float64, w int) string {
	if m.rates[dir].Unavailable {
		return fitLine(label+"  --  awaiting current server window", w)
	}
	return rateLine(label, m.displayRates[dir], scale, w)
}
