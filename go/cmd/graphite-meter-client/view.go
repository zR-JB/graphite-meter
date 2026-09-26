package main

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"strings"
	"time"

	"charm.land/bubbles/v2/key"
	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
)

const twoColumnMin = 100

func (m model) View() tea.View {
	content, cursor := m.render()
	v := tea.NewView(content)
	v.AltScreen = true
	v.WindowTitle = "Graphite Meter · " + m.statusLabel()
	v.Cursor = cursor
	if m.running() {
		v.ProgressBar = tea.NewProgressBar(tea.ProgressBarDefault, m.progress())
	}
	return v
}

const minWidth, minHeight = 40, 12

func (m model) size() (int, int) { return max(m.width, minWidth), max(m.height, minHeight) }

func (m model) popupWidth() int {
	w, _ := m.size()
	return min(w-4, 84)
}

type frame struct {
	top, footer string
	body        []string
	bodyH       int
	offset      int
}

func (m model) layout() frame {
	w, h := m.size()
	inner := w - 2
	gap := h >= 20
	top := m.header(inner)
	if gap {
		top += "\n"
	}
	if m.run == nil {
		top += "\n" + m.tabBar(inner)
		if gap {
			top += "\n"
		}
	}
	f := frame{top: top}
	f.bodyH = max(h-lipgloss.Height(top)-lipgloss.Height(m.footer(inner, false)), 1)
	if m.run != nil {
		f.body = strings.Split(m.runView(inner, f.bodyH), "\n")
	} else {
		f.body = strings.Split(m.setupView(inner), "\n")
	}
	limit := max(len(f.body)-f.bodyH, 0)
	f.offset = m.scroll
	if m.run == nil && m.scroll == 0 {
		f.offset = m.row + 3 - f.bodyH
	}
	f.offset = min(max(f.offset, 0), limit)
	f.footer = m.footer(inner, limit > 0)
	return f
}

func (m model) render() (string, *tea.Cursor) {
	if m.width > 0 && (m.width < minWidth || m.height < minHeight) {
		notice := fmt.Sprintf("Enlarge the terminal to at least %d×%d.", minWidth, minHeight)
		return lipgloss.Place(m.width, m.height, lipgloss.Center, lipgloss.Center,
			lipgloss.NewStyle().Width(m.width).Align(lipgloss.Center).Render(notice)), nil
	}
	w, _ := m.size()
	f := m.layout()
	body := strings.Join(f.body[f.offset:min(f.offset+f.bodyH, len(f.body))], "\n")
	body = lipgloss.PlaceVertical(f.bodyH, lipgloss.Top, body)
	screen := lipgloss.NewStyle().Padding(0, 1).Render(f.top + "\n" + body + "\n" + f.footer)
	pw := m.popupWidth()
	var title, content string
	switch {
	case m.popup == popupDetails:
		title, content = "Details", m.detailsViewport().View()
	case m.popup == popupServers:
		title, content = m.serverChooserView(pw-4, f.bodyH-2)
	case m.edit != nil:
		title, content = m.editView()
	case m.auth != nil && m.run == nil:
		title, content = m.signInView(pw - 4)
	default:
		return screen, nil
	}
	box := m.st.panel(title, content, pw, min(lipgloss.Height(content)+2, f.bodyH))
	screen, x, y := overlay(screen, box, w, lipgloss.Height(f.top), f.bodyH)
	if m.edit == nil || m.popup != popupNone {
		return screen, nil
	}
	cursor := m.edit.input.Cursor()
	if cursor != nil {
		cursor.X, cursor.Y = cursor.X+x+2, y+1
	}
	return screen, cursor
}

func (m *model) scrollBody(msg tea.KeyPressMsg) {
	f := m.layout()
	step := 1
	switch msg.String() {
	case "pgup", "pgdown":
		step = max(f.bodyH-1, 1)
	case "home", "end":
		step = len(f.body)
	}
	if reverse(msg) || msg.String() == "pgup" || msg.String() == "home" {
		step = -step
	}
	m.scroll = min(max(f.offset+step, 0), max(len(f.body)-f.bodyH, 0))
}

func (m model) progress() int {
	var done, total float64
	for _, s := range m.run.stages {
		total += s.duration.Seconds()
		switch s.state {
		case stageDone:
			done += s.duration.Seconds()
		case stageMeasuring:
			done += min(m.now.Sub(s.since), s.duration).Seconds()
		}
	}
	return int(done / max(total, 1) * 100)
}

func (m model) header(w int) string {
	left := m.st.title.Render("Graphite Meter")
	right := m.st.pill.Render(m.statusLabel())
	spacer := strings.Repeat(" ", max(1, w-lipgloss.Width(left)-lipgloss.Width(right)))
	context := m.cfg.BaseURL
	if m.run != nil && m.run.details != nil {
		var names []string
		for _, server := range m.run.details.Servers {
			names = append(names, serverLabel(server.Server.Name, server.Server.Location))
		}
		context = strings.Join(names, ", ")
	}
	line := m.st.muted.Render("native client "+goclient.Version+"  ") + m.st.accent.Render(context)
	return fit(left+spacer+right+"\n"+line, w)
}

func (m model) footer(w int, overflow bool) string {
	notice := m.st.muted.Render(m.notice)
	if m.run != nil && m.run.err != nil {
		notice = m.st.err.Render(errorText(m.run.err))
	}
	if m.help.ShowAll {
		return fit(notice+"\n"+m.help.FullHelpView(m.FullHelp()), w)
	}
	bindings := m.ShortHelp()
	if overflow && m.popup == popupNone && m.edit == nil && m.auth == nil {
		bindings = slices.Insert(bindings, 1, keys.page)
	}
	line := m.help.ShortHelpView(bindings)
	for lipgloss.Width(line) > w {
		drop := len(bindings) - 3
		for drop > 0 && bindings[drop].Help() == keys.page.Help() {
			drop--
		}
		if drop <= 0 {
			drop = slices.IndexFunc(bindings, func(b key.Binding) bool { return b.Help() == keys.help.Help() })
		}
		if drop < 0 {
			break
		}
		bindings = slices.Delete(bindings, drop, drop+1)
		line = m.help.ShortHelpView(bindings)
	}
	return fit(notice+"\n"+line, w)
}

func columns(w int) (int, int, bool) {
	if w < twoColumnMin {
		return w, w, false
	}
	left := (w - 1) * 3 / 5
	return left, w - 1 - left, true
}

func join(left, right string, side bool) string {
	if side {
		return lipgloss.JoinHorizontal(lipgloss.Top, left, " ", right)
	}
	return left + "\n" + right
}

func (m model) setupView(w int) string {
	lw, rw, side := columns(w)
	rows, plan := m.sectionView(lw-4), m.planView(rw-4)
	panelH := 0
	if side {
		panelH = max(lipgloss.Height(rows), lipgloss.Height(plan)) + 2
	}
	sections := m.st.panel(sections[m.section].label, rows, lw, panelH)
	return join(sections, m.st.panel("Test plan", plan, rw, panelH), side)
}

func (m model) tabBar(w int) string {
	parts := make([]string, len(sections))
	for i, s := range sections {
		style := m.st.tab
		if i == m.section {
			style = m.st.activeTab
		}
		parts[i] = style.Render(s.label)
	}
	line := lipgloss.JoinHorizontal(lipgloss.Left, parts...)
	return fit(line+m.st.border.Render(strings.Repeat("─", max(w-lipgloss.Width(line), 0))), w)
}

func (m model) sectionView(w int) string {
	rows := sections[m.section].rows
	labelWidth := 0
	for _, s := range rows {
		labelWidth = max(labelWidth, len(s.row(m).label))
	}
	labelWidth = min(labelWidth, max(w/2, 12))
	var lines []string
	for i, s := range rows {
		row := s.row(m)
		value := m.st.value.Render(row.value)
		if row.inert {
			value = m.st.muted.Render(row.value)
		}
		label := pad(ansi.Truncate(row.label, labelWidth, "…"), labelWidth)
		line := m.st.text.Render(label) + "  " + value + "  " + m.st.muted.Render(row.note)
		if i == m.row {
			line = "› " + m.st.selected.Render(ansi.Truncate(line, w-2, "…"))
		} else {
			line = "  " + line
		}
		lines = append(lines, line)
	}
	return strings.Join(lines, "\n")
}

func (m model) planView(w int) string {
	var lines []string
	if m.prepare == prepareChecking && m.preparedRun == nil {
		lines = append(lines, m.spin.View()+" "+m.st.muted.Render("Checking paths…"))
	}
	rows := m.readiness()
	nameWidth := 0
	for _, r := range rows {
		nameWidth = max(nameWidth, len([]rune(serverLabel(r.server.Name, r.server.Location))))
	}
	for _, r := range rows {
		glyph := m.st.ok.Render("●")
		switch r.label {
		case "Checking…":
			glyph = m.spin.View()
		case "Sign in":
			glyph = m.st.warn.Render("○")
		case "Unavailable":
			glyph = m.st.err.Render("✗")
		}
		name := pad(serverLabel(r.server.Name, r.server.Location), nameWidth)
		lines = append(lines, glyph+" "+name+"  "+m.st.text.Render(r.label))
		if r.detail != "" {
			lines = append(lines, m.st.warn.PaddingLeft(2).Width(max(w, 4)).Render(r.detail))
		}
	}
	if m.prepareErr != "" {
		lines = append(lines, m.st.warn.Width(max(w, 4)).Render(m.prepareErr))
	}
	if m.canUseAvailable() && m.auth == nil {
		lines = append(lines, m.st.muted.Render("u Use available servers"))
	}
	throughput, latency := m.pathSummaries()
	lines = append(lines, "", m.st.text.Render(pad("Throughput", 11))+throughput,
		m.st.text.Render(pad("Latency", 11))+latency, "", m.st.text.Render("Stages"))
	for _, stage := range m.cfg.Plan() {
		lines = append(lines, "  "+pad(stageLabels[stage.Name], 14)+m.st.muted.Render(fmtSetting(stage.Duration)))
	}
	if len(m.cfg.Plan()) == 0 {
		lines = append(lines, "  "+m.st.warn.Render("No stages selected"))
	}
	return strings.Join(lines, "\n")
}

func serverLabel(name, location string) string {
	if location == "" {
		return name
	}
	return name + " · " + location
}

func (m model) pathSummaries() (throughput, latency string) {
	var throughputs, latencies []string
	if m.preparedRun != nil {
		for _, s := range m.preparedRun.Servers {
			if c := s.Connection; c != nil {
				t := c.ThroughputTarget
				throughputs = appendUnique(throughputs, goclient.ConnectionSummary(t.Transport, t.Protocol, t.TLS()))
				if l := c.LatencyTarget; l != nil {
					latencies = appendUnique(latencies, goclient.ConnectionSummary(l.Transport, l.Protocol, l.TLS()))
				}
			}
		}
	}
	value := func(summaries []string) string {
		if len(summaries) == 0 {
			return m.st.muted.Render(missing)
		}
		style := m.st.value
		if !m.preparedRun.FreshFor(m.cfg) {
			style = m.st.muted
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

func (m model) signInView(w int) (string, string) {
	issuer := m.serverName(m.challengedServer())
	if issuer == "" {
		issuer = m.cfg.BaseURL
	}
	status := "Open sign-in page"
	if m.auth.opened {
		status = "Waiting for approval…"
	}
	waited := m.now.Sub(m.auth.since)
	code := lipgloss.NewStyle().Border(lipgloss.RoundedBorder()).BorderForeground(m.st.border.GetForeground()).
		Padding(0, 1).Render(m.st.value.Render(m.auth.pending.Code))
	lines := []string{
		m.spin.View() + " " + m.st.accent.Render(status),
		lipgloss.JoinHorizontal(lipgloss.Center, m.st.text.Render("Match this code "), code),
		m.st.muted.Render(fmt.Sprintf("waited %s · expires in %s",
			fmtClock(waited), fmtClock(goclient.AuthorizationTimeout-waited))), "",
		m.st.muted.Width(w).Render(m.auth.pending.BrowserURL),
	}
	return "Sign in to " + issuer, strings.Join(lines, "\n")
}

func (m model) editView() (string, string) {
	note := m.st.muted.Render("enter applies · esc cancels")
	if m.edit.err != "" {
		note = m.st.err.Render(m.edit.err)
	}
	return m.edit.row.row(m).label, m.edit.input.View() + "\n" + note
}

func (m model) runView(w, h int) string {
	results := m.resultsView(w - 4)
	resultsH := 0
	if results != "" {
		resultsH = lipgloss.Height(results) + 2
	}
	lw, rw, side := columns(w)
	if side {
		lw, rw = w*2/5, w-1-w*2/5
	}
	test := m.testView(lw-4, !side)
	liveH := max(h-resultsH, 9)
	testH := liveH
	if !side {
		testH = lipgloss.Height(test) + 2
		liveH = max(h-resultsH-testH, 7)
	}
	live := m.st.panel("Live · "+m.statusLabel(), m.liveView(rw-4, liveH-2), rw, liveH)
	top := join(m.st.panel("Test", test, lw, testH), live, side)
	if results == "" {
		return top
	}
	title := "Results"
	if m.multipleRunServers() {
		title += " · Combined throughput · latency to " + m.serverName(m.run.focus)
	}
	return top + "\n" + m.st.panel(title, results, w, resultsH)
}

func (m model) testView(w int, compact bool) string {
	r := m.run
	var lines []string
	field := func(label, value string) { lines = append(lines, m.st.text.Render(pad(label, 11))+value) }
	switch {
	case compact:
		return strings.Join(m.stageTrack(w), "\n")
	case r.details == nil && m.running():
		field("Servers", m.spin.View()+m.st.muted.Render(" Checking paths…"))
	case r.details == nil:
		field("Servers", m.st.muted.Render(missing))
	default:
		var names, throughputs []string
		streams, latency := "", missing
		for _, server := range r.details.Servers {
			names = append(names, server.Server.Name)
			t := server.Throughput
			throughputs = appendUnique(throughputs, goclient.ConnectionSummary(t.Transport, t.Protocol, t.TLS()))
			streams = m.cfg.TransferStreams.Label(t.Protocol, t.Transport)
			if l := server.LatencyTarget; server.Server.ID == r.focus && l != nil {
				latency = goclient.ConnectionSummary(l.Transport, l.Protocol, l.TLS())
			}
		}
		servers := strings.Join(names, ", ")
		if len(names) > 1 {
			servers += m.st.muted.Render(" · Combined")
			streams = "per server · " + streams
		}
		field("Servers", m.st.value.Render(servers))
		field("Throughput", m.st.value.Render(strings.Join(throughputs, " / ")))
		field("Latency", m.st.value.Render(latency))
		field("Streams", m.st.value.Render(streams))
		timing := "warmup " + fmtSetting(m.cfg.Warmup) + " · ping " + cadenceLabel(m.cfg.PingInterval) +
			" / loaded " + cadenceLabel(m.cfg.LoadedPingInterval)
		field("Timing", m.st.value.Render(timing))
	}
	lines = append(lines, "")
	return strings.Join(append(lines, m.stageTrack(w)...), "\n")
}

func (m model) stageTrack(w int) []string {
	barW := min(max(w-34, 6), 30)
	var lines []string
	for _, s := range m.run.stages {
		name := m.st.text.Render(pad(stageLabels[s.name], 14))
		elapsed := m.now.Sub(s.since)
		switch s.state {
		case stagePreparing:
			lines = append(lines, name+m.spin.View()+m.st.muted.Render(" checking paths"))
		case stageWarmup:
			lines = append(lines, name+m.spin.View()+m.st.muted.Render(" warmup ")+m.st.value.Render(fmtClock(elapsed)))
		case stageMeasuring:
			clock := m.st.value.Render(fmtClock(min(elapsed, s.duration)))
			clock += m.st.muted.Render(" / " + fmtSetting(s.duration))
			lines = append(lines, name+m.st.bar(elapsed.Seconds(), s.duration.Seconds(), barW)+"  "+clock)
		case stageDone:
			lines = append(lines, name+m.st.ok.Render("✓ ")+m.st.muted.Render(fmtSetting(s.duration)))
		case stageStopped:
			lines = append(lines, name+m.st.err.Render("✗ ")+m.st.muted.Render("stopped"))
		case stagePending:
			if !m.running() {
				lines = append(lines, name+m.st.muted.Render(missing+" not run"))
				continue
			}
			fallthrough
		default:
			lines = append(lines, name+m.st.muted.Render("○ "+fmtSetting(s.duration)))
		}
	}
	return lines
}

func (m model) liveView(w, h int) string {
	r := m.run
	i := slices.IndexFunc(r.plan, func(s goclient.StagePlan) bool { return s.Name == r.stage })
	switch {
	case i < 0 && !m.running():
		return m.st.muted.Render(missing)
	case i < 0 || r.phase == goclient.PhasePreparing && m.running():
		return m.spin.View() + m.st.muted.Render(" Checking paths…")
	}
	stage := r.plan[i]
	loaded := len(stage.Directions) > 0 && m.cfg.LoadedLatency
	var readings []string
	var lines []series
	for _, dir := range stage.Directions {
		label := map[goclient.Direction]string{goclient.Down: "↓ ", goclient.Up: "↑ "}[dir]
		sample, sampled := r.rates[dir]
		value := m.st.value.Render(fmtRate(r.shown[dir]))
		switch {
		case !sampled || !m.running() || r.phase != goclient.PhaseMeasuring:
			value = m.st.muted.Render(missing)
		case sample.Unavailable:
			value = m.st.muted.Render(missing + " window restarting")
		}
		readings = append(readings, m.st.text.Render(label)+value)
		style := map[goclient.Direction]lipgloss.Style{goclient.Down: m.st.down, goclient.Up: m.st.up}[dir]
		lines = append(lines, series{style, r.history[dir]})
	}
	if len(stage.Directions) == 0 || loaded {
		label := "Loaded latency "
		if len(stage.Directions) == 0 {
			label = "Idle latency "
		}
		value := m.st.muted.Render(missing)
		if sample, ok := r.latest[r.focus]; ok && m.running() {
			value = m.st.value.Render(fmtMs(sample.RTT))
		}
		if streak := r.timeouts[r.focus]; streak > 0 && m.running() {
			style := m.st.warn
			if streak >= 3 {
				style = m.st.err
			}
			value += style.Render(fmt.Sprintf("  probe timeout ×%d", streak))
		}
		readings = append(readings, m.st.text.Render(label)+value)
	}
	out := []string{strings.Join(readings, "   ")}
	if m.multipleRunServers() {
		out = append(out, m.st.muted.Render("Latency to "+m.serverName(r.focus)+" · l switches server"))
	}
	chartH := h - len(out)
	span := m.now.Sub(r.started).Seconds()
	rtt := []series{{m.st.rtt, r.rtt[r.focus]}}
	msLabel := func(v float64) string { return fmtMs(time.Duration(v)) }
	switch {
	case chartH < 5:
	case len(lines) == 0:
		out = append(out, m.st.chart(rtt, r.marks, msLabel, span, w, chartH))
	case loaded && chartH >= 12:
		out = append(out, m.st.chart(lines, r.marks, fmtRate, span, w, chartH-5),
			m.st.chart(rtt, nil, msLabel, span, w, 5))
	default:
		out = append(out, m.st.chart(lines, r.marks, fmtRate, span, w, chartH))
	}
	return strings.Join(out, "\n")
}

func (m model) resultsView(w int) string {
	r := m.run
	latency := r.latencyPopulations()
	var idle *goclient.LatencyStats
	if population, ok := latency[goclient.StageLatency]; ok && population.Latency.Count > 0 {
		idle = &population.Latency
	}
	var throughput, latencyRows [][]string
	var notes []string
	note := func(label string, facts []string) {
		for i, line := range wrapParts(facts, w-2) {
			if i == 0 {
				line = label + ": " + line
				if lipgloss.Width(line) > w {
					notes = append(notes, m.st.muted.Render(label+":"))
					line = "  " + strings.TrimPrefix(line, label+": ")
				}
			} else {
				line = "  " + line
			}
			notes = append(notes, m.st.muted.Render(line))
		}
	}
	failed := func(label string, err error) {
		switch {
		case errors.Is(err, context.Canceled):
			notes = append(notes, m.st.warn.Render(label+" stopped."))
		case err != nil:
			notes = append(notes, m.st.err.Render(label+" incomplete: "+errorText(err)))
		}
	}
	unmeasured := func(i int) string {
		if r.stages[i].state == stageStopped {
			return "Stopped"
		}
		return missing
	}
	added, measured := false, false
	for i, stage := range r.plan {
		if len(stage.Directions) > 0 {
			row, found := []string{compactStage(stage.Name), "", ""}, false
			for _, result := range r.results {
				if result.Stage != stage.Name {
					continue
				}
				found = true
				rate := missing
				if !result.Unavailable {
					rate = fmtRate(result.MeanBps)
				}
				row[map[goclient.Direction]int{goclient.Down: 1, goclient.Up: 2}[result.Direction]] = rate
				note(directionLabel(result), throughputFacts(result))
				failed(directionLabel(result), result.Err)
			}
			measured = measured || found
			if !found && !m.running() {
				row[1], found = unmeasured(i), true
			}
			if found {
				throughput = append(throughput, row)
			}
		}
		population, ok := latency[stage.Name]
		switch {
		case ok:
			measured = true
			base := idle
			if stage.Name == goclient.StageLatency {
				base = nil
			}
			cells := latencyCells(population.Latency, base)
			added = added || cells[1] != ""
			latencyRows = append(latencyRows, append([]string{compactPopulation(stage.Name)}, cells...))
			label := populationLabel(stage.Name)
			note(label, latencyFacts(population.Latency))
			if timing := population.Latency.ReflectorTiming; timing != nil {
				note(reflectorTimingFacts(timing))
			}
			failed(label, population.Err)
		case len(stage.Directions) == 0 && !m.running():
			latencyRows = append(latencyRows, []string{compactPopulation(stage.Name), unmeasured(i)})
		}
	}
	if !measured {
		return ""
	}
	if added {
		notes = append(notes, m.st.muted.Render("Added: loaded median minus idle median."))
	}
	var parts []string
	if len(throughput) > 0 {
		parts = append(parts, m.st.grid([]string{"Throughput", "Download", "Upload"}, throughput, w))
	}
	if len(latencyRows) > 0 {
		headers := []string{"Latency", "Median", "Added", "p95", "Jitter", "Probe timeouts"}
		for i, row := range latencyRows {
			latencyRows[i] = append(row, make([]string, len(headers)-len(row))...)
		}
		parts = append(parts, m.st.grid(headers, latencyRows, w))
	}
	return strings.Join(append(parts, strings.Join(notes, "\n")), "\n")
}

func (m model) finalReport() string {
	if m.run == nil || m.running() {
		return ""
	}
	w, _ := m.size()
	lines := []string{"Graphite Meter · " + outcomeLabels[m.run.outcome]}
	if results := m.resultsView(w); results != "" {
		lines = append(lines, results)
	}
	if m.multipleRunServers() {
		lines = append(lines, "", m.detailsView(w, false))
	}
	if m.run.err != nil {
		lines = append(lines, errorText(m.run.err))
	}
	report := strings.Split(ansi.Strip(strings.Join(lines, "\n")), "\n")
	for i, line := range report {
		report[i] = strings.TrimRight(line, " ")
	}
	return strings.Join(report, "\n")
}
