package main

import (
	"fmt"
	"slices"
	"strings"

	"github.com/charmbracelet/lipgloss"
	"github.com/charmbracelet/x/ansi"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
)

const (
	// shellMargin is shellStyle's horizontal margin.
	shellMargin = 2
	// panelBorderWidth is the column pair a panel's rounded border draws outside its lipgloss width.
	panelBorderWidth = 2
	// gutterWidth separates two side-by-side panels.
	gutterWidth  = 2
	twoColumnMin = 115
)

func (m model) View() string {
	w := m.innerWidth()
	var body string
	switch {
	case m.detailsOpen:
		body = m.detailsOverlay(w)
	case m.serverChooser:
		body = m.serverChooserView(w)
	case m.run != nil:
		body = m.runView(w)
	default:
		body = m.setupView(w)
	}
	return shellStyle.Render(m.header(w) + "\n\n" + body + "\n" + m.helpView())
}

func (m model) innerWidth() int {
	return max(m.width-2*shellMargin, 40)
}

func fitLine(s string, w int) string {
	if lipgloss.Width(s) <= w {
		return s
	}
	return lipgloss.NewStyle().MaxWidth(w).Render(s)
}

func fitBlock(s string, w int) string {
	lines := strings.Split(s, "\n")
	for i, line := range lines {
		lines[i] = fitLine(line, w)
	}
	return strings.Join(lines, "\n")
}

func (m model) header(w int) string {
	left := titleStyle.Render("Graphite Meter")
	right := pillStyle.Render(m.statusLabel())
	spacer := strings.Repeat(" ", max(1, w-lipgloss.Width(left)-lipgloss.Width(right)))
	context := m.cfg.BaseURL
	if m.run != nil && m.run.details != nil {
		var names []string
		for _, server := range m.run.details.Servers {
			names = append(names, serverLabel(server.Server.Name, server.Server.Location))
		}
		context = strings.Join(names, ", ")
	}
	return fitLine(left+spacer+right, w) + "\n" + fitLine(mutedStyle.Render("native client "+goclient.Version+"  ")+accentStyle.Render(context), w)
}

// panels lays two panels side by side on a wide terminal and stacks them otherwise.
func panels(w int, left func(int) string, right func(int) string) string {
	if w < twoColumnMin {
		inner := w - panelBorderWidth
		return panelStyle.Width(inner).Render(fitBlock(left(inner-4), inner-4)) + "\n\n" + panelStyle.Width(inner).Render(fitBlock(right(inner-4), inner-4))
	}
	inner := w - gutterWidth - 2*panelBorderWidth
	leftW := inner * 12 / 20
	return lipgloss.JoinHorizontal(lipgloss.Top,
		panelStyle.Width(leftW).Render(fitBlock(left(leftW-4), leftW-4)), "  ",
		panelStyle.Width(inner-leftW).Render(fitBlock(right(inner-leftW-4), inner-leftW-4)))
}

func (m model) setupView(w int) string {
	var b strings.Builder
	b.WriteString(m.tabBar(w))
	b.WriteString("\n\n")
	// A pending approval is what the screen waits on, so it goes above the settings it blocks.
	if auth := m.signInView(); auth != "" {
		b.WriteString(panelStyle.Width(w - panelBorderWidth).Render(fitBlock(auth, w-6)))
		b.WriteString("\n\n")
	}
	b.WriteString(panels(w, m.sectionView, m.planView))
	if m.notice != "" {
		b.WriteString("\n\n" + fitLine(mutedStyle.Render(m.notice), w))
	}
	return b.String()
}

func (m model) tabBar(w int) string {
	parts := make([]string, len(sections))
	for i, s := range sections {
		style := tabStyle
		if i == m.section {
			style = activeTabStyle
		}
		parts[i] = style.Render(s.label)
	}
	line := lipgloss.JoinHorizontal(lipgloss.Left, parts...)
	if lipgloss.Width(line) < w {
		line += subtleRuleStyle.Render(strings.Repeat("─", w-lipgloss.Width(line)))
	}
	// Clipping the trailing tabs holds the block at w; a longer line pads every other line past the terminal.
	return fitLine(line, w)
}

func (m model) sectionView(w int) string {
	rows := sections[m.section].rows
	labelWidth := 0
	for _, id := range rows {
		labelWidth = max(labelWidth, len(m.setupRow(id).label))
	}
	lines := []string{accentStyle.Render(sections[m.section].label)}
	for i, id := range rows {
		row := m.setupRow(id)
		value, note := valueStyle.Render(row.value), mutedStyle.Render(row.note)
		if row.inert {
			value = mutedStyle.Render(row.value)
		}
		if m.edit.row != nil && *m.edit.row == id {
			value, note = m.edit.input.View(), mutedStyle.Render("enter applies · esc cancels")
			if m.edit.err != "" {
				note = errorStyle.Render(m.edit.err)
			}
		}
		line := labelStyle.Render(pad(row.label, labelWidth)) + "  " + value + "  " + note
		if i == m.row {
			line = "› " + selectedStyle.Render(fitLine(line, w-2))
		} else {
			line = "  " + line
		}
		lines = append(lines, line)
	}
	return strings.Join(lines, "\n")
}

// planView is the setup's readiness: one row per selected server, the resolved paths, and the stage order.
func (m model) planView(w int) string {
	lines := []string{accentStyle.Render("Test plan")}
	if m.prepare == prepareChecking && m.preparedRun == nil {
		lines = append(lines, m.spin.View()+" "+mutedStyle.Render("Checking paths…"))
	}
	rows := m.readiness()
	nameWidth := 0
	for _, r := range rows {
		nameWidth = max(nameWidth, len([]rune(serverLabel(r.server.Name, r.server.Location))))
	}
	for _, r := range rows {
		glyph := successStyle.Render("●")
		switch r.label {
		case "Checking…":
			glyph = m.spin.View()
		case "Sign in":
			glyph = warnStyle.Render("○")
		case "Unavailable":
			glyph = errorStyle.Render("✗")
		}
		lines = append(lines, glyph+" "+pad(serverLabel(r.server.Name, r.server.Location), nameWidth)+"  "+labelStyle.Render(r.label))
		if r.detail != "" {
			lines = append(lines, lipgloss.NewStyle().MarginLeft(4).Width(max(1, w-4)).Render(warnStyle.Render(r.detail)))
		}
	}
	if m.prepareErr != "" {
		lines = append(lines, warnStyle.Render(m.prepareErr))
	}
	if m.canUseAvailable() {
		lines = append(lines, mutedStyle.Render("u Use available servers"))
	}
	throughput, latency := m.pathSummaries()
	lines = append(lines, "", labelStyle.Render(pad("Throughput", 11))+throughput, labelStyle.Render(pad("Latency", 11))+latency, "", labelStyle.Render("Stages"))
	for _, stage := range m.cfg.Plan() {
		lines = append(lines, "  "+pad(stageLabels[stage.Name], 14)+mutedStyle.Render(fmtSetting(stage.Duration)))
	}
	if len(m.cfg.Plan()) == 0 {
		lines = append(lines, "  "+warnStyle.Render("No stages selected"))
	}
	return strings.Join(lines, "\n")
}

func serverLabel(name, location string) string {
	if location == "" {
		return name
	}
	return name + " · " + location
}

// pathSummaries names the checked paths; servers that resolved differently are listed together.
func (m model) pathSummaries() (throughput, latency string) {
	var throughputs, latencies []string
	if m.preparedRun != nil {
		for _, s := range m.preparedRun.Servers {
			if c := s.Connection; c != nil {
				throughputs = appendUnique(throughputs, c.ThroughputSummary())
				latencies = appendUnique(latencies, c.LatencySummary())
			}
		}
	}
	value := func(summaries []string) string {
		if len(summaries) == 0 {
			return mutedStyle.Render(missing)
		}
		style := valueStyle
		if !m.preparedRun.FreshFor(m.cfg) {
			style = mutedStyle
		}
		return style.Render(strings.Join(summaries, " / "))
	}
	return value(throughputs), value(latencies)
}

func appendUnique(list []string, value string) []string {
	if slices.Contains(list, value) {
		return list
	}
	return append(list, value)
}

func (m model) signInView() string {
	if m.auth == nil {
		return ""
	}
	issuer := m.serverName(m.authServerID)
	if m.authServerID == "" {
		issuer = m.cfg.BaseURL
	}
	waited := m.now.Sub(m.authSince)
	status := "Open sign-in page"
	if m.authOpened {
		status = "Waiting for approval…"
	}
	code := lipgloss.JoinHorizontal(lipgloss.Center,
		labelStyle.Render("Match this code ")+codeStyle.Render(m.auth.Code),
		mutedStyle.Render(fmt.Sprintf("  waited %s · expires in %s", fmtClock(waited), fmtClock(goclient.AuthorizationTimeout-waited))),
	)
	return strings.Join([]string{
		m.spin.View() + " " + accentStyle.Render("Sign in to "+issuer+" · "+status),
		"",
		code,
		mutedStyle.Render("enter Open sign-in page · esc Cancel sign-in"),
		mutedStyle.Render(m.auth.BrowserURL),
	}, "\n")
}

func (m model) runView(w int) string {
	var b strings.Builder
	b.WriteString(panels(w, m.testView, m.liveView))
	if lines := m.resultLines(w - 6); len(lines) > 0 {
		b.WriteString("\n\n")
		b.WriteString(panelStyle.Width(w - panelBorderWidth).Render(fitBlock(strings.Join(lines, "\n"), w-6)))
	}
	if m.multipleRunServers() {
		b.WriteString("\n\n" + fitLine(m.outcomeNotice()+mutedStyle.Render(" · d Details"), w))
	}
	if m.run.err != nil {
		b.WriteString("\n\n" + fitLine(errorStyle.Render(m.run.err.Error()), w))
	} else if m.notice != "" {
		b.WriteString("\n\n" + fitLine(mutedStyle.Render(m.notice), w))
	}
	return b.String()
}

// testView names what is being measured over which paths, then the stage track.
func (m model) testView(w int) string {
	r := m.run
	lines := []string{accentStyle.Render("Test")}
	field := func(label, value string) { lines = append(lines, labelStyle.Render(pad(label, 11))+value) }
	if r.details == nil {
		field("Servers", m.spin.View()+mutedStyle.Render(" Checking paths…"))
	} else {
		var names, throughputs []string
		streams, latency := "", missing
		for _, server := range r.details.Servers {
			names = append(names, server.Server.Name)
			t := server.Throughput
			throughputs = appendUnique(throughputs, goclient.ConnectionSummary(t.Transport, t.Protocol, t.TLS))
			streams = m.cfg.TransferStreams.Label(t.Protocol, t.Transport)
			if l := server.LatencyTarget; server.Server.ID == r.focus && l != nil {
				latency = goclient.ConnectionSummary(l.Transport, l.Protocol, l.TLS)
				if len(r.details.Servers) > 1 {
					latency = server.Server.Name + " · " + latency
				}
			}
		}
		servers := strings.Join(names, ", ")
		if len(names) > 1 {
			servers += mutedStyle.Render(" · Combined")
			streams = "per server · " + streams
		}
		field("Servers", valueStyle.Render(servers))
		field("Throughput", valueStyle.Render(strings.Join(throughputs, " / ")))
		field("Latency", valueStyle.Render(latency))
		field("Streams", valueStyle.Render(streams))
		field("Timing", valueStyle.Render("warmup "+fmtSetting(m.cfg.Warmup)+" · ping "+cadenceLabel(m.cfg.PingInterval)))
	}
	lines = append(lines, "")
	return strings.Join(append(lines, m.stageTrack(w)...), "\n")
}

func (m model) stageTrack(w int) []string {
	barW := min(max(w-34, 8), 40)
	var lines []string
	for _, s := range m.run.stages {
		name := labelStyle.Render(pad(stageLabels[s.name], 14))
		elapsed := m.now.Sub(s.since)
		switch s.state {
		case stagePreparing:
			lines = append(lines, name+m.spin.View()+mutedStyle.Render(" checking paths"))
		case stageWarmup:
			lines = append(lines, name+m.spin.View()+mutedStyle.Render(" warmup ")+valueStyle.Render(fmtClock(elapsed)))
		case stageMeasuring:
			lines = append(lines, name+renderBar(elapsed.Seconds(), s.duration.Seconds(), barW)+"  "+valueStyle.Render(fmtClock(min(elapsed, s.duration)))+mutedStyle.Render(" / "+fmtSetting(s.duration)))
		case stageDone:
			lines = append(lines, name+successStyle.Render("✓ ")+mutedStyle.Render(fmtSetting(s.duration)))
		case stageStopped:
			lines = append(lines, name+errorStyle.Render("✗ ")+mutedStyle.Render("stopped"))
		default:
			lines = append(lines, name+mutedStyle.Render("○ "+fmtSetting(s.duration)))
		}
	}
	return lines
}

// liveView draws only what the current stage measures: its directions' rates and its latency population.
func (m model) liveView(w int) string {
	r := m.run
	lines := []string{accentStyle.Render("Live")}
	i := slices.IndexFunc(r.plan, func(s goclient.StagePlan) bool { return s.Name == r.stage })
	if i < 0 || r.phase == goclient.PhasePreparing && m.running() {
		return strings.Join(append(lines, m.spin.View()+mutedStyle.Render(" Checking paths…")), "\n")
	}
	stage := r.plan[i]
	scale := max(r.peaks[goclient.Down], r.peaks[goclient.Up])
	for _, dir := range stage.Directions {
		label := map[goclient.Direction]string{goclient.Down: "Download", goclient.Up: "Upload"}[dir]
		value := valueStyle.Render(fmt.Sprintf("%13s", fmtRate(r.displayRates[dir])))
		if r.rates[dir].Unavailable {
			value = mutedStyle.Render(fmt.Sprintf("%13s", missing) + "  window restarting")
		}
		lines = append(lines, labelStyle.Render(pad(label, 9))+renderBar(r.displayRates[dir], scale, max(12, w-26))+value)
	}
	if len(stage.Directions) == 0 || m.cfg.LoadedLatency {
		label := "Loaded latency"
		if len(stage.Directions) == 0 {
			label = "Idle latency"
		}
		value := mutedStyle.Render("waiting")
		if sample, ok := r.latest[r.focus]; ok {
			value = valueStyle.Render(fmtMs(sample.RTT))
		}
		if streak := r.timeoutStreak[r.focus]; streak > 0 {
			style := warnStyle
			if streak >= 3 {
				style = errorStyle
			}
			value += style.Render(fmt.Sprintf("  probe timeout ×%d", streak))
		}
		lines = append(lines, labelStyle.Render(pad(label, 16))+value)
	}
	if m.multipleRunServers() {
		lines = append(lines, mutedStyle.Render("Latency to "+r.serverName(r.focus)+" · l switches server"))
	}
	return strings.Join(lines, "\n")
}

// resultLines lists each stage's populations in run order. Throughput is Combined across servers;
// latency belongs to the focused server, with added latency measured against its own idle median.
func (m model) resultLines(w int) []string {
	r := m.run
	latency := r.latencyPopulations()
	var idle *goclient.LatencyStats
	if population, ok := latency[goclient.StageLatency]; ok && population.Latency.Count > 0 {
		idle = &population.Latency
	}
	var scale float64
	for _, result := range r.results {
		scale = max(scale, result.MeanBps, result.PeakBps)
	}
	title := "Results"
	if m.multipleRunServers() {
		title += " · Combined throughput · latency to " + r.serverName(r.focus)
	}
	lines := []string{accentStyle.Render(title)}
	barW := min(max(w-78, 8), 32)
	for _, stage := range r.plan {
		for _, result := range r.results {
			if result.Stage == stage.Name {
				lines = append(lines, throughputLines(result, scale, barW, w)...)
			}
		}
		if population, ok := latency[stage.Name]; ok {
			base := idle
			if stage.Name == goclient.StageLatency {
				base = nil
			}
			lines = append(lines, latencyLines(population, base, w)...)
		}
	}
	if len(lines) == 1 {
		return nil
	}
	return lines
}

func throughputLines(result goclient.Result, scale float64, barW, w int) []string {
	rate := missing
	if !result.Unavailable {
		rate = fmtRate(result.MeanBps)
	}
	var facts []string
	if result.PeakBps > 0 {
		facts = append(facts, "peak "+fmtRate(result.PeakBps))
	}
	facts = append(facts, fmtBytes(result.TotalBytes))
	if result.Elapsed > 0 {
		facts = append(facts, fmtClock(result.Elapsed))
	}
	if result.Samples > 0 {
		facts = append(facts, fmt.Sprintf("%d samples", result.Samples))
	}
	if result.ReceiverTimed() {
		facts = append(facts, "receiver-timed")
	}
	head := labelStyle.Render(pad(directionLabel(result), 14)) + renderBar(result.MeanBps, scale, barW) + "  " + valueStyle.Render(fmt.Sprintf("%13s", rate)) + "  "
	indent := strings.Repeat(" ", lipgloss.Width(head))
	var lines []string
	for i, line := range wrapParts(facts, w-len(indent)) {
		lines = append(lines, map[bool]string{true: head, false: indent}[i == 0]+mutedStyle.Render(line))
	}
	if result.Err != nil {
		lines = append(lines, errorStyle.Render("  Incomplete: "+result.Err.Error()))
	}
	return lines
}

func latencyLines(result goclient.Result, idle *goclient.LatencyStats, w int) []string {
	const labelWidth = 27
	var lines []string
	for i, line := range wrapParts(latencyParts(result.Latency, idle), w-labelWidth) {
		label := strings.Repeat(" ", labelWidth)
		if i == 0 {
			label = labelStyle.Render(pad(populationLabel(result.Stage), labelWidth))
		}
		lines = append(lines, label+valueStyle.Render(line))
	}
	if timing := reflectorTimingSummary(result.Latency.ReflectorTiming); timing != "" {
		lines = append(lines, mutedStyle.Width(max(1, w-2)).MarginLeft(2).Render(timing))
	}
	if result.Err != nil {
		lines = append(lines, errorStyle.Render("  Incomplete: "+result.Err.Error()))
	}
	return lines
}

// finalReport is the plain-text record printed after the program leaves the alternate screen.
func (m model) finalReport() string {
	if m.run == nil || m.running() {
		return ""
	}
	w := m.innerWidth()
	lines := []string{"Graphite Meter · " + outcomeLabels[m.run.outcome]}
	lines = append(lines, m.resultLines(w)...)
	if m.multipleRunServers() {
		lines = append(lines, "", m.detailsView(w))
	}
	if m.run.err != nil {
		lines = append(lines, "", m.run.err.Error())
	}
	return ansi.Strip(strings.Join(lines, "\n"))
}

// helpView is the footer. The model is the key map it renders, so the listing follows the screen on show.
func (m model) helpView() string {
	m.help.Width = m.innerWidth()
	return fitBlock(m.help.View(m), m.innerWidth())
}
