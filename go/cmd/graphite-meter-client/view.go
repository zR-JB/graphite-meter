package main

import (
	"fmt"
	"strings"

	"github.com/charmbracelet/lipgloss"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
)

var (
	shellStyle      = lipgloss.NewStyle().Margin(1, 2)
	titleStyle      = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("230")).Background(lipgloss.Color("57")).Padding(0, 1)
	pillStyle       = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("16")).Background(lipgloss.Color("86")).Padding(0, 1)
	panelStyle      = lipgloss.NewStyle().Border(lipgloss.RoundedBorder()).BorderForeground(lipgloss.Color("240")).Padding(1, 2)
	activeTabStyle  = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("230")).Background(lipgloss.Color("63")).Padding(0, 1)
	tabStyle        = lipgloss.NewStyle().Foreground(lipgloss.Color("245")).Padding(0, 1)
	selectedStyle   = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("230")).Background(lipgloss.Color("238"))
	labelStyle      = lipgloss.NewStyle().Foreground(lipgloss.Color("245"))
	valueStyle      = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("86"))
	mutedStyle      = lipgloss.NewStyle().Foreground(lipgloss.Color("244"))
	errorStyle      = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("203"))
	accentStyle     = lipgloss.NewStyle().Foreground(lipgloss.Color("111"))
	warnStyle       = lipgloss.NewStyle().Foreground(lipgloss.Color("215"))
	successStyle    = lipgloss.NewStyle().Foreground(lipgloss.Color("120"))
	subtleRuleStyle = lipgloss.NewStyle().Foreground(lipgloss.Color("239"))
)

func (m model) View() string {
	w := m.width
	if w < 76 {
		w = 76
	}
	inner := w - 4
	if inner > 118 {
		inner = 118
	}

	var b strings.Builder
	b.WriteString(m.header(inner))
	b.WriteString("\n")
	if m.mode == modeRun {
		b.WriteString(m.runView(inner))
	} else {
		b.WriteString(m.configView(inner))
	}
	b.WriteString("\n")
	b.WriteString(m.helpView())
	return shellStyle.Render(b.String())
}

func (m model) header(w int) string {
	status := "ready"
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

// splitColumns divides w into two panels separated by a two-cell gutter. Below
// 96 cells neither panel stays readable side by side, so both take the full
// width and the caller stacks them.
func splitColumns(w int) (leftW, rightW int, twoCol bool) {
	if w < 96 {
		return w, w, false
	}
	leftW = (w - 2) / 2
	return leftW, w - leftW - 2, true
}

func (m model) configView(w int) string {
	var b strings.Builder
	b.WriteString(m.tabBar(w))
	b.WriteString("\n\n")

	leftW, rightW, twoCol := splitColumns(w)
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
	return line
}

func (m model) sectionView(w int) string {
	switch m.section {
	case sectionServers:
		return m.serversView(w)
	case sectionStages:
		return m.stagesView(w)
	case sectionTiming:
		return m.timingView(w)
	case sectionNetwork:
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
	if m.prepareStatus == "ready" {
		label = successStyle.Render(label)
	} else if m.prepareStatus == "failed" {
		label = warnStyle.Render(label)
	}
	return m.listWithTitle("Connection readiness", []string{label}, w)
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
	lines := []string{
		accentStyle.Render("Resolved Plan"),
		labelStyle.Render("Status     ") + valueStyle.Render(m.prepareStatus),
		labelStyle.Render("Throughput ") + valueStyle.Render(throughput),
		labelStyle.Render("Latency    ") + valueStyle.Render(latency),
		labelStyle.Render("Observed   ") + valueStyle.Render(emptyDash(observed)),
		"",
		mutedStyle.Render("Run order"),
	}
	if m.prepareError != "" {
		lines = append(lines, warnStyle.Render(m.prepareError), mutedStyle.Render("Press v to retry."), "")
	}
	for _, line := range runOrder(m.cfg) {
		lines = append(lines, "  "+line)
	}
	return lipgloss.JoinVertical(lipgloss.Left, lines...)
}

func (m model) runView(w int) string {
	leftW, rightW, twoCol := splitColumns(w)
	summary := panelStyle.Width(leftW).Render(m.summary)
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

func (m model) summaryView() string {
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
		labelStyle.Render("Progress  ") + valueStyle.Render(" selected throughput path"),
		labelStyle.Render("Streams ") + valueStyle.Render(m.cfg.TransferStreams.Label(m.target)) + mutedStyle.Render("  warmup "+m.cfg.Warmup.String()+"  ping "+m.cfg.PingInterval.String()),
	}
	if m.complete {
		lines = append(lines, "", successStyle.Render("Finished. Press m for menus or r to run again."))
	}
	return lipgloss.JoinVertical(lipgloss.Left, lines...)
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

// isLatencyResult reports whether a result carries latency percentiles rather
// than throughput figures.
func isLatencyResult(r goclient.Result) bool {
	return r.Latency.Count > 0 || r.Stage == "latency"
}

func (m model) helpView() string {
	if m.edit.kind != editNone {
		return mutedStyle.Render("type or paste • ←/→ home/end move • enter apply • esc cancel • ctrl+c quit")
	}
	if m.mode == modeRun {
		return mutedStyle.Render("c/esc cancel • m menus after finish • r rerun after finish • q quit")
	}
	return mutedStyle.Render("tab switch menu • ↑/↓ select • enter edit/toggle/select • v recheck • r run • q quit")
}
