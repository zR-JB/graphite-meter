package main

import (
	"fmt"
	"strings"

	"github.com/charmbracelet/bubbles/help"
	"github.com/charmbracelet/lipgloss"
	"github.com/muesli/termenv"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
)

const (
	// shellMargin is shellStyle's horizontal margin. Mouse coordinates are
	// absolute, so hit-testing adds it back.
	shellMargin = 2
	// panelContentTop is the distance from a panel's first line to its first
	// content line: the rounded border plus panelStyle's padding line.
	panelContentTop = 2
)

// layout records where View last drew the clickable parts of the configure
// screen. View takes the model by value, so the record lives behind a pointer
// every copy shares. Positions are absolute terminal cells.
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

// markRow records the menu row about to be appended to a section body of the
// given lines. Rows render in row order, so their positions land in that order
// too. The panel wraps its content at w, so a row's position is the wrapped
// height of everything above it rather than the line count.
func (l *layout) markRow(lines []string, w int) {
	wrap := lipgloss.NewStyle().Width(w)
	offset := 0
	for _, line := range lines {
		offset += lipgloss.Height(wrap.Render(line))
	}
	l.rows = append(l.rows, l.rowTop+offset)
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

// innerWidth is the content width inside the shell margin: floored so both
// panels stay legible, capped so lines do not sprawl on wide terminals.
func (m model) innerWidth() int {
	return clamp(m.width-4, 72, 118)
}

func (m model) header(w int) string {
	status := m.prepareStatus
	if m.mode == modeRun {
		status = emptyDash(m.status)
	}
	left := titleStyle.Render("Graphite Meter")
	right := pillStyle.Render(status)
	spacer := strings.Repeat(" ", max(1, w-lipgloss.Width(left)-lipgloss.Width(right)))
	line := left + spacer + right

	target := m.cfg.BaseURL
	if m.server != "" {
		target = m.server
	}
	return line + "\n" + mutedStyle.Render("native go client "+goclient.Version) + "  " + accentStyle.Render(target)
}

// splitColumns divides w into two bordered panels separated by a two-cell
// gutter. Each panel's border draws two columns outside its lipgloss width, so
// the content widths split w minus the gutter and both border pairs — the pair
// then renders exactly w columns wide. Below 96 cells neither panel stays
// readable side by side, so both take the full width and the caller stacks
// them.
func splitColumns(w int) (leftW, rightW int, twoCol bool) {
	if w < 96 {
		return w, w, false
	}
	inner := w - 6
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
	// The menu panel's rendered span includes the border columns outside its
	// lipgloss width.
	m.lay.rowRight = shellMargin + leftW + 2
	menu := panelStyle.Width(leftW).Render(m.sectionView(leftW - 4))
	summary := panelStyle.Width(rightW).Render(m.planView(rightW - 4))
	if twoCol {
		b.WriteString(lipgloss.JoinHorizontal(lipgloss.Top, menu, "  ", summary))
	} else {
		b.WriteString(menu)
		b.WriteString("\n\n")
		b.WriteString(summary)
	}
	if m.notice != "" {
		b.WriteString("\n\n")
		b.WriteString(mutedStyle.Render(m.notice))
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
	return line
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
		m.lay.markRow(lines, w)
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
	m.lay.markRow(lines, w)
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
	throughput := targetChoiceLabel(m.cfg.ThroughputTarget)
	latency := targetChoiceLabel(m.cfg.LatencyTarget)
	if m.prepared.FreshFor(m.cfg) {
		throughput = m.prepared.ThroughputSummary()
		latency = m.prepared.LatencySummary()
	}
	rows := []string{
		valueLine("Throughput endpoint", throughput, "independent path"),
		valueLine("Latency endpoint", latency, "independent path"),
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

// editError trails the field being edited with the reason its last commit was
// rejected, so the answer sits where the eye already is.
func (m model) editError() string {
	if m.edit.err == "" {
		return ""
	}
	return "  " + errorStyle.Render(m.edit.err)
}

func (m model) listWithTitle(title string, rows []string, w int) string {
	lines := []string{accentStyle.Render(title)}
	for i, row := range rows {
		m.lay.markRow(lines, w)
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
	summary := panelStyle.Width(leftW).Render(m.summaryView(leftW - 4))
	live := panelStyle.Width(rightW).Render(m.liveView(rightW - 4))
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
		b.WriteString(panelStyle.Width(w).Render(m.resultsView(w - 4)))
	}
	if m.err != nil {
		b.WriteString("\n\n")
		b.WriteString(errorStyle.Render(m.err.Error()))
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
// its configured duration, so its bar is determinate; the warmup window is
// adapted to the measured RTT inside the engine and never reported, so warmup
// counts up under an indeterminate spinner instead of down.
func (m model) timelineView(w int) []string {
	if len(m.stages) == 0 {
		return nil
	}
	lines := []string{mutedStyle.Render("Stages")}
	barW := clamp(w-38, 8, 24)
	for _, s := range m.stages {
		name := labelStyle.Render(fmt.Sprintf("%-14s", s.name))
		switch s.state {
		case stageWarmup:
			lines = append(lines,
				name+accentStyle.Render("◐ ")+m.spin.View(),
				mutedStyle.Render(fmt.Sprintf("  %-12s %s", "warmup", fmtClock(m.now.Sub(s.since)))),
			)
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
		rateLine("download", m.rates[goclient.Down].BytesPerSec, scale, w),
		rateLine("upload  ", m.rates[goclient.Up].BytesPerSec, scale, w),
		latencyLine(m.latency),
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
	barW := clamp(w-56, 10, 30)

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
