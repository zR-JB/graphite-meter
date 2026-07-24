package main

import (
	"fmt"
	"strings"

	"github.com/charmbracelet/bubbles/help"
	"github.com/charmbracelet/lipgloss"
	"github.com/muesli/termenv"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

const (
	// shellMargin is shellStyle's horizontal margin. Mouse coordinates are
	// absolute, so hit-testing adds it back.
	shellMargin = 2
	// panelContentTop is the distance from a panel's first line to its first
	// content line: the rounded border plus panelStyle's padding line.
	panelContentTop = 2
)

// layout records where View draws the clickable parts of the configure screen.
// View takes the model by value, so the record lives behind a pointer every
// copy shares. Positions are absolute terminal cells.
type layout struct {
	tabY     int
	tabs     []span // one per section, in tab order
	rowTop   int    // absolute y of the first line of the menu panel's body
	rowRight int    // first column past the menu panel
	rows     []int  // absolute y of each menu row, in row order
}

type span struct{ from, to int }

func (l *layout) reset() {
	l.tabs = l.tabs[:0]
	l.rows = l.rows[:0]
}

// markRow records the position of the menu row about to be appended to lines.
// fitBlock truncates every body line to the panel width, so a line occupies
// exactly one row and the position is the line count.
func (l *layout) markRow(lines []string) {
	l.rows = append(l.rows, l.rowTop+len(lines))
}

func (m model) View() string {
	w := m.innerWidth()
	m.lay.reset()

	var b strings.Builder
	b.WriteString(m.header(w))
	b.WriteString("\n")
	if m.mode == modeRun {
		b.WriteString(m.runView(w))
	} else {
		// shellStyle's top margin pushes every drawn line down by one.
		b.WriteString(m.configView(w, 1+strings.Count(b.String(), "\n")))
	}
	b.WriteString("\n")
	b.WriteString(m.helpView())
	return shellStyle.Render(b.String())
}

// innerWidth is the content width inside the shell margin. Bars and columns
// grow with the terminal, down to a floor under which no layout stays legible.
func (m model) innerWidth() int {
	return max(m.width-4, 40)
}

// fitLine truncates one rendered line to w cells, ANSI-aware, so panel content
// never wraps: a cramped terminal loses a line's tail, not its layout. A line
// already within width returns untouched, skipping lipgloss's render pipeline
// for the common case.
func fitLine(s string, w int) string {
	if lipgloss.Width(s) <= w {
		return s
	}
	return lipgloss.NewStyle().MaxWidth(w).Render(s)
}

// fitBlock applies fitLine to every line of a panel body. It walks the block
// without allocating a line slice and returns the input unchanged when nothing
// overflows, so the common in-width case allocates nothing.
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
	// panelBorderWidth is the column pair a panel's rounded border draws
	// outside its lipgloss width.
	panelBorderWidth = 2
	// gutterWidth separates two side-by-side panels.
	gutterWidth = 2
	// twoColumnMin is the narrowest w where two panels stay readable side by
	// side. Below it the caller stacks them.
	twoColumnMin = 96
)

// splitColumns sizes panel content so a rendered pair, or one stacked panel,
// spans exactly w columns.
func splitColumns(w int) (leftW, rightW int, twoCol bool) {
	if w < twoColumnMin {
		return w - panelBorderWidth, w - panelBorderWidth, false
	}
	inner := w - gutterWidth - 2*panelBorderWidth
	leftW = inner / 2
	return leftW, inner - leftW, true
}

func (m model) configView(w, top int) string {
	var b strings.Builder
	b.WriteString(m.tabBar(w, top))
	b.WriteString("\n\n")

	leftW, rightW, twoCol := splitColumns(w)
	// The menu panel opens two lines under the tab bar, whatever the terminal
	// width: the summary panel is beside it or below it, never above.
	m.lay.rowTop = top + 2 + panelContentTop
	m.lay.rowRight = shellMargin + leftW + panelBorderWidth
	menu := panelStyle.Width(leftW).Render(fitBlock(m.sectionView(leftW-4), leftW-4))
	summary := panelStyle.Width(rightW).Render(fitBlock(m.planView(rightW-4), rightW-4))
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

func (m model) tabBar(w, y int) string {
	m.lay.tabY = y
	x := shellMargin
	parts := make([]string, 0, len(sectionLabels))
	for i, label := range sectionLabels {
		style := tabStyle
		if section(i) == m.section {
			style = activeTabStyle
		}
		tab := style.Render(label)
		m.lay.tabs = append(m.lay.tabs, span{from: x, to: x + lipgloss.Width(tab)})
		x += lipgloss.Width(tab)
		parts = append(parts, tab)
	}
	line := lipgloss.JoinHorizontal(lipgloss.Left, parts...)
	if lipgloss.Width(line) < w {
		line += subtleRuleStyle.Render(strings.Repeat("─", w-lipgloss.Width(line)))
	}
	// Clipping the trailing tabs holds the shell block at w. A longer line pads
	// every other line past the terminal.
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

func (m model) serversView(w int) string {
	lines := []string{accentStyle.Render("Server Selection")}
	active := activePreset(m.cfg.BaseURL)
	for i, preset := range serverPresets {
		mark := " "
		if i == active {
			mark = "●"
		}
		line := fmt.Sprintf("%s %-12s %s", mark, preset.name, mutedStyle.Render(preset.url))
		m.lay.markRow(lines)
		lines = append(lines, m.menuLine(i, line, w))
		lines = append(lines, mutedStyle.Render("  "+preset.note))
	}
	custom := "○ Custom URL  " + m.cfg.BaseURL
	if active == -1 {
		custom = "● Custom URL  " + m.cfg.BaseURL
	}
	if m.edit.kind == editURL {
		custom = "● Custom URL  " + m.edit.input.View() + m.editError()
	}
	m.lay.markRow(lines)
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
		rows[m.row] = valueLine(timingLabel(m.row), m.edit.input.View(), "editing") + m.editError()
	}
	return m.listWithTitle("Timing", rows, w)
}

func (m model) networkView(w int) string {
	caps := m.capabilities()
	throughputChoices := originChoices(caps.ThroughputTargets, func(t wire.ThroughputTarget) string { return t.Origin })
	latencyChoices := originChoices(caps.LatencyTargets, func(t wire.LatencyTarget) string { return t.Origin })
	resolvedThroughput, resolvedLatency := "", ""
	if m.prepared.FreshFor(m.cfg) {
		resolvedThroughput = m.prepared.ThroughputSummary()
		resolvedLatency = m.prepared.LatencySummary()
	}
	rows := []string{
		endpointRow("Throughput endpoint", m.cfg.ThroughputTarget, throughputChoices, resolvedThroughput),
		endpointRow("Latency endpoint", m.cfg.LatencyTarget, latencyChoices, resolvedLatency),
		valueLine("Throughput protocol", m.cfg.ThroughputProtocol, "negotiated endpoints"),
		valueLine("Auto H1 max", fmt.Sprintf("%d", m.cfg.TransferStreams.AutomaticMax), "per direction"),
		valueLine("Streams", m.cfg.TransferStreams.Label(m.cfg.ThroughputProtocol), "0 automatic; 1–128 forced"),
		toggleLine("Unsafe: skip TLS verification", m.cfg.InsecureSkipTLSVerify, "advanced; all native TLS requests"),
		warnStyle.Render("Reset to defaults"),
	}
	if m.edit.field == "auto-streams" {
		rows[3] = valueLine("Auto H1 max", m.edit.input.View(), "editing") + m.editError()
	}
	if m.edit.field == "streams" {
		rows[4] = valueLine("Streams", m.edit.input.View(), "editing") + m.editError()
	}
	return m.listWithTitle("Connections", rows, w)
}

func (m model) runMenuView(w int) string {
	label := "Start measurement · " + m.prepareStatus
	switch m.prepareStatus {
	case "ready":
		label = accentStyle.Render(label)
	case "failed":
		label = warnStyle.Render(label)
	default:
		label = m.spin.View() + " " + label
	}
	return m.listWithTitle("Start", []string{label}, w)
}

// editError trails the edited field with the reason its last commit fails, so
// the answer sits where the eye already is.
func (m model) editError() string {
	if m.edit.err == "" {
		return ""
	}
	return "  " + errorStyle.Render(m.edit.err)
}

func (m model) listWithTitle(title string, rows []string, w int) string {
	lines := []string{accentStyle.Render(title)}
	for i, row := range rows {
		m.lay.markRow(lines)
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
	lines := []string{accentStyle.Render("Connection readiness")}
	lines = append(lines, m.checklistView()...)
	if m.prepareError != "" {
		lines = append(lines, warnStyle.Render(m.prepareError), mutedStyle.Render("Press v to retry."))
	}
	lines = append(lines, m.authView()...)
	lines = append(lines,
		"",
		labelStyle.Render("Throughput ")+valueStyle.Render(throughput),
		labelStyle.Render("Latency    ")+valueStyle.Render(latency),
		labelStyle.Render("Observed   ")+valueStyle.Render(emptyDash(observed)),
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

// authView is the browser approval wait: where to approve, the code the page
// asks the operator to match, and how much of the polling window is left.
func (m model) authView() []string {
	if m.auth == nil {
		return nil
	}
	waited := m.now.Sub(m.authSince)
	return []string{
		"",
		m.spin.View() + " " + accentStyle.Render("Approve this client in the browser"),
		mutedStyle.Render(m.auth.BrowserURL),
		codeStyle.Render(m.auth.Code),
		mutedStyle.Render("waiting " + fmtClock(waited) + " · expires in " + fmtClock(authWait-waited)),
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
	if len(m.results) > 0 {
		b.WriteString("\n\n")
		b.WriteString(panelStyle.Width(w - 2).Render(fitBlock(m.resultsView(w-6), w-6)))
	}
	if m.err != nil {
		b.WriteString("\n\n")
		b.WriteString(fitLine(errorStyle.Render(m.err.Error()), w))
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
		labelStyle.Render("Streams ") + valueStyle.Render(m.cfg.TransferStreams.Label(m.target)) + mutedStyle.Render("  warmup "+m.cfg.Warmup.String()+"  ping "+m.cfg.PingInterval.String()),
		"",
	}
	lines = append(lines, m.timelineView(w)...)
	if m.complete {
		lines = append(lines, "", successStyle.Render("✓")+accentStyle.Render(" Finished. Press esc for setup or r to run again."))
	}
	return lipgloss.JoinVertical(lipgloss.Left, lines...)
}

// timelineView is the run's stage timeline. A measuring stage runs for exactly
// its configured duration, so its bar is determinate. The warmup window is
// never reported, so warmup counts up under an indeterminate spinner.
func (m model) timelineView(w int) []string {
	if len(m.stages) == 0 {
		return nil
	}
	lines := []string{mutedStyle.Render("Stages")}
	barW := clamp(w-38, 8, 40)
	for _, s := range m.stages {
		name := labelStyle.Render(fmt.Sprintf("%-14s", s.name))
		switch s.state {
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
		rateLine("download", m.displayRates[goclient.Down], scale, w),
		rateLine("upload  ", m.displayRates[goclient.Up], scale, w),
		latencyLine(m.latency, m.lostStreak),
	}
	return lipgloss.JoinVertical(lipgloss.Left, lines...)
}

// rateScale is the live bars' denominator: the larger session peak across both
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

	var scale float64
	for _, r := range m.results {
		if !isLatencyResult(r) && r.PeakBps > scale {
			scale = r.PeakBps
		}
	}
	barW := clamp(w-56, 10, 48)

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
		bar := renderBar(r.MeanBps, scale, barW, false)
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

// finalReport is what the process prints once the alt screen is torn down,
// which takes everything the TUI drew with it. The ASCII profile keeps the
// surviving scrollback plain text.
func (m model) finalReport() string {
	if !m.complete || len(m.results) == 0 {
		return ""
	}
	lipgloss.SetColorProfile(termenv.Ascii)
	return m.resultsView(m.innerWidth())
}

// isLatencyResult reports whether a result carries latency percentiles rather
// than throughput figures.
func isLatencyResult(r goclient.Result) bool {
	return r.Latency.Count > 0 || r.Stage == "latency"
}

// helpView is the footer. The model is the key map it renders, so the listing
// follows whichever screen is on show.
func (m model) helpView() string {
	m.help.Width = m.innerWidth()
	return m.help.View(m)
}

// newHelp is the footer renderer, dressed in this program's styles rather than
// the bubble's defaults.
func newHelp() help.Model {
	h := help.New()
	h.Styles.ShortKey, h.Styles.FullKey = labelStyle, labelStyle
	h.Styles.ShortDesc, h.Styles.FullDesc = mutedStyle, mutedStyle
	h.Styles.ShortSeparator, h.Styles.FullSeparator = subtleRuleStyle, subtleRuleStyle
	h.Styles.Ellipsis = subtleRuleStyle
	return h
}
