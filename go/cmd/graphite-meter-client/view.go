package main

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"strings"
	"time"

	"charm.land/bubbles/v2/viewport"
	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
)

const twoColumnMin = 100

func (m model) View() tea.View {
	v := tea.NewView(m.render())
	v.AltScreen = true
	v.WindowTitle = "Graphite Meter · " + m.statusLabel()
	switch {
	case m.next != nil:
		v.ProgressBar = tea.NewProgressBar(tea.ProgressBarIndeterminate, 0)
	case m.running():
		v.ProgressBar = tea.NewProgressBar(tea.ProgressBarDefault, m.progress())
	}
	return v
}

const minWidth, minHeight = 40, 12

func (m model) size() (int, int) { return max(m.width, minWidth), max(m.height, minHeight) }

type frame struct {
	top, footer string
	body        []string
	bodyH       int
}

func (m model) layout() frame {
	w, h := m.size()
	inner := w - 2
	top := m.header(inner)
	if h > 24 {
		top += "\n"
	}
	f := frame{top: top}
	f.bodyH = max(h-lipgloss.Height(top)-lipgloss.Height(m.footer(inner, false)), 1)
	var body string
	switch {
	case m.popup == popupDetails:
		body = m.st.panel("Details", m.detailsView(inner-4, true), inner, 0)
	case m.popup == popupServers:
		title, content := m.serverChooserView(inner-4, f.bodyH-2)
		body = m.st.panel(title, content, inner, 0)
	case m.auth != nil && m.run == nil:
		title, content := m.signInView(inner - 4)
		body = m.st.panel(title, content, inner, 0)
	case m.run != nil:
		body = m.runView(inner, f.bodyH)
	default:
		body = m.setupView(inner)
	}
	f.body = strings.Split(body, "\n")
	f.footer = m.footer(inner, len(f.body) > f.bodyH)
	return f
}

func (m model) bodyViewport(f frame) viewport.Model {
	vp := m.body
	w, _ := m.size()
	vp.SetWidth(w - 2)
	vp.SetHeight(f.bodyH)
	vp.SetContentLines(f.body)
	return vp
}

func (m model) render() string {
	if m.width > 0 && (m.width < minWidth || m.height < minHeight) {
		notice := fmt.Sprintf("Enlarge the terminal to at least %d×%d.", minWidth, minHeight)
		return lipgloss.Place(m.width, m.height, lipgloss.Center, lipgloss.Center,
			lipgloss.NewStyle().Width(m.width).Align(lipgloss.Center).Render(notice))
	}
	f := m.layout()
	return lipgloss.NewStyle().Padding(0, 1).Render(f.top + "\n" + m.bodyViewport(f).View() + "\n" + f.footer)
}

func (m *model) scrollBody(msg tea.KeyPressMsg) {
	m.body = m.bodyViewport(m.layout())
	switch msg.String() {
	case "pgup":
		m.body.PageUp()
	case "pgdown":
		m.body.PageDown()
	case "home":
		m.body.GotoTop()
	case "end":
		m.body.GotoBottom()
	default:
		if reverse(msg) {
			m.body.ScrollUp(1)
		} else {
			m.body.ScrollDown(1)
		}
	}
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
	pill := m.st.pill
	if m.run != nil && !m.running() {
		pill = m.st.outcome[m.run.outcome]
	}
	right := pill.Render(m.statusLabel())
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
	switch {
	case m.edit != nil && m.edit.err != "":
		notice = m.st.err.Render(m.edit.err)
	case m.run != nil && m.run.err != nil:
		notice = m.st.err.Render(errorText(m.run.err))
	case m.notice == "" && m.run == nil && m.popup == popupNone && m.auth == nil:
		notice = m.st.muted.Render(m.currentRow().row(m).help)
	}
	if m.help.ShowAll {
		return fit(notice+"\n"+m.help.FullHelpView(m.FullHelp()), w)
	}
	bindings := m.ShortHelp()
	if overflow && m.popup == popupNone && m.edit == nil && m.auth == nil {
		bindings = slices.Insert(bindings, 1, keys.page)
	}
	line := m.help.ShortHelpView(bindings)
	for lipgloss.Width(line) > w && len(bindings) > 2 {
		bindings = slices.Delete(bindings, len(bindings)-2, len(bindings)-1)
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
	rows, _ := m.setupList(lw - 4)
	plan := m.planView(rw - 4)
	panelH := 0
	if side {
		panelH = max(lipgloss.Height(rows), lipgloss.Height(plan)) + 2
	}
	return join(m.st.panel("Setup", rows, lw, panelH), m.st.panel("Servers", plan, rw, panelH), side)
}

func (m model) setupList(w int) (string, int) {
	rows := m.rows()
	labelWidth := 0
	for _, s := range rows {
		labelWidth = max(labelWidth, lipgloss.Width(s.row(m).label))
	}
	labelWidth = min(labelWidth, max(w/2, 12))
	var lines []string
	selected, i := 0, 0
	for g, group := range setupGroups {
		if g > 0 {
			lines = append(lines, "")
		}
		if group.label != "" {
			lines = append(lines, m.st.heading.Render(group.label))
		}
		for _, s := range group.rows {
			if i == len(rows) || rows[i] != s {
				break
			}
			if i == m.row {
				selected = len(lines)
			}
			lines = append(lines, m.settingLine(s, i == m.row, labelWidth, w))
			i++
		}
	}
	return strings.Join(lines, "\n"), selected
}

func (m model) settingLine(s *setting, focused bool, labelWidth, w int) string {
	var line string
	if s == startRow {
		line = m.st.button(s.label, focused) + "  " + m.startNote()
	} else {
		row := s.row(m)
		value := m.st.value.Render(row.value)
		switch {
		case m.edit != nil && m.edit.row == s:
			value = m.edit.input.View()
		case row.inert:
			value = m.st.muted.Render(row.value)
		}
		line = m.st.text.Render(pad(ansi.Truncate(row.label, labelWidth, "…"), labelWidth)) + "  " + value
		if focused {
			line = m.st.selected.Render(ansi.Truncate(line, w-2, "…"))
		}
	}
	if focused {
		return "› " + line
	}
	return "  " + line
}

func (m model) startNote() string {
	plan := m.cfg.Plan()
	total := time.Duration(len(plan)) * m.cfg.Warmup
	for _, stage := range plan {
		total += stage.Duration
	}
	switch err := m.cfg.Validate(); {
	case err != nil:
		return m.st.warn.Render(err.Error())
	case m.prepare == prepareSignIn:
		return m.st.warn.Render("sign in first; v requests a new code")
	case m.prepare == prepareChecking:
		return m.spin.View() + m.st.muted.Render(" checking paths")
	}
	return m.st.muted.Render(fmt.Sprintf("%d stages · about %s", len(plan), fmtSetting(total.Round(time.Second))))
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
		glyph := map[pathState]string{pathReady: m.st.ok.Render("●"), pathChecking: m.spin.View(),
			pathStale: m.st.warn.Render("○"), pathFailed: m.st.err.Render("✗")}[r.state]
		name := pad(serverLabel(r.server.Name, r.server.Location), nameWidth)
		lines = append(lines, glyph+" "+name+"  "+m.st.text.Render(pathLabels[r.state]))
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
		m.st.text.Render(pad("Latency", 11))+latency)
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
				throughputs = appendUnique(throughputs, connectionSummary(t.Transport, t.Protocol, t.TLS(), false))
				if l := c.LatencyTarget; l != nil {
					latencies = appendUnique(latencies, connectionSummary(l.Transport, l.Protocol, l.TLS(), true))
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

func (m model) runView(w, h int) string {
	results := ""
	if !m.run.live() {
		results, _ = m.resultsView(w - 4)
	}
	if results == "" {
		return m.stageView(w, h)
	}
	title := "Results"
	if m.multipleRunServers() {
		title += " · Combined throughput · latency to " + m.serverName(m.run.focus)
	}
	bottom := m.st.panel(title, results, w, 0)
	if rw := max(lipgloss.Width(results), lipgloss.Width(title)+2) + 4; w >= twoColumnMin && w-1-rw >= 30 {
		fields := strings.Join(m.testFields(w-1-rw-4), "\n")
		bottomH := max(lipgloss.Height(results), lipgloss.Height(fields)) + 2
		bottom = join(m.st.panel(title, results, rw, bottomH), m.st.panel("Test", fields, w-1-rw, bottomH), true)
	}
	if timelineH := h - lipgloss.Height(bottom); timelineH >= 8 {
		return m.timelinePanel(w, timelineH) + "\n" + bottom
	}
	return bottom
}

func (m model) stageView(w, h int) string {
	lw, rw, side := columns(w)
	if side {
		lw, rw = w*2/5, w-1-w*2/5
	}
	test := m.testView(lw-4, !side)
	testH := lipgloss.Height(test) + 2
	liveH := h
	if !side {
		liveH -= testH
	}
	switch {
	case side && (m.run.live() || liveH >= 9):
		liveH = max(liveH, testH, 9)
		return join(m.st.panel("Test", test, lw, liveH), m.timelinePanel(rw, liveH), true)
	case m.run.live() || liveH >= 9:
		return m.st.panel("Test", test, lw, 0) + "\n" + m.timelinePanel(rw, max(liveH, 7))
	}
	return m.st.panel("Test", m.testView(w-4, !side), w, 0)
}

func (m model) timelinePanel(w, h int) string {
	title := "Timeline"
	if m.run.live() {
		title += " · " + m.statusLabel()
	}
	return m.st.panel(title, m.liveView(w-4, h-2), w, h)
}

func (m model) testView(w int, compact bool) string {
	if compact {
		return strings.Join(m.stageTrack(w), "\n")
	}
	return strings.Join(append(append(m.testFields(w), ""), m.stageTrack(w)...), "\n")
}

func (m model) testFields(w int) []string {
	r := m.run
	label := m.st.text.Render(pad("Servers", 11))
	var lines []string
	field := func(name, value string) {
		for i, line := range wrapParts(strings.Split(value, " · "), max(w-11, 12)) {
			if i > 0 {
				name = ""
			}
			lines = append(lines, m.st.text.Render(pad(name, 11))+m.st.value.Render(line))
		}
	}
	switch {
	case r.details == nil && r.live():
		lines = append(lines, label+m.spin.View()+m.st.muted.Render(" Checking paths…"))
	case r.details == nil:
		lines = append(lines, label+m.st.muted.Render(missing))
	default:
		var names, throughputs []string
		streams, latency := "", missing
		for _, server := range r.details.Servers {
			names = append(names, server.Server.Name)
			t := server.Throughput
			throughputs = appendUnique(throughputs, connectionSummary(t.Transport, t.Protocol, t.TLS(), false))
			streams = streamsLabel(m.cfg.TransferStreams, t.Protocol, t.Transport)
			if l := server.LatencyTarget; server.Server.ID == r.focus && l != nil {
				latency = connectionSummary(l.Transport, l.Protocol, l.TLS(), true)
			}
		}
		servers := strings.Join(names, ", ")
		if len(names) > 1 {
			servers += " · Combined"
			streams = "per server · " + streams
		}
		field("Servers", servers)
		field("Throughput", strings.Join(throughputs, " / "))
		field("Latency", latency)
		field("Streams", streams)
		field("Timing", "warmup "+fmtSetting(m.cfg.Warmup)+" · latency cadence "+cadenceLabel(m.cfg.PingInterval)+
			" · loaded cadence "+cadenceLabel(m.cfg.LoadedPingInterval))
	}
	return lines
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
			value := m.headline(s.name)
			if value == "" {
				value = m.st.muted.Render(fmtSetting(s.duration))
			}
			lines = append(lines, name+m.st.ok.Render("✓ ")+value)
		case stagePartial:
			value := strings.TrimSpace(m.headline(s.name) + " " + m.st.muted.Render(stageStatusLabels[s.state]))
			lines = append(lines, name+m.st.warn.Render("! ")+value)
		case stageFailed, stageStopped:
			lines = append(lines, name+m.st.err.Render("✗ ")+m.st.muted.Render(stageStatusLabels[s.state]))
		case stagePending:
			if !m.run.live() {
				lines = append(lines, name+m.st.muted.Render(missing+" "+stageStatusLabels[s.state]))
				continue
			}
			fallthrough
		default:
			lines = append(lines, name+m.st.muted.Render("○ "+fmtSetting(s.duration)))
		}
	}
	return lines
}

func (m model) headline(stage goclient.Stage) string {
	var parts []string
	if rates := m.run.meanRates(stage); rates != "" {
		parts = append(parts, m.st.value.Render(rates))
	}
	if p, ok := m.run.latencyPopulations()[stage]; ok && stage == goclient.StageLatency && p.HasMedian() {
		parts = append(parts, m.st.value.Render(fmtMs(p.Latency.P50))+m.st.muted.Render(" median"))
	}
	return strings.Join(parts, "  ")
}

func (m model) liveView(w, h int) string {
	r := m.run
	i := slices.IndexFunc(r.plan, func(s goclient.StagePlan) bool { return s.Name == r.stage })
	switch {
	case i < 0 && !r.live():
		return m.st.muted.Render(missing)
	case i < 0 || r.phase == goclient.PhasePreparing && r.live():
		return m.spin.View() + m.st.muted.Render(" Checking paths…")
	}
	stage := r.plan[i]
	dirs := stage.Directions
	if !r.live() {
		dirs = slices.DeleteFunc([]goclient.Direction{goclient.Down, goclient.Up}, func(d goclient.Direction) bool {
			return len(r.history[d].points) == 0
		})
	}
	loaded := len(dirs) > 0 && m.cfg.LoadedLatency
	var lines []series
	for _, dir := range dirs {
		style := map[goclient.Direction]lipgloss.Style{goclient.Down: m.st.down, goclient.Up: m.st.up}[dir]
		lines = append(lines, series{style, r.history[dir].points})
	}
	var out []string
	if r.live() {
		out = append(out, m.readings(stage))
	}
	if m.multipleRunServers() {
		out = append(out, m.st.muted.Render("Latency to "+m.serverName(r.focus)+" · l switches server"))
	}
	chartH := h - len(out)
	span := m.now.Sub(r.started).Seconds()
	rtt := []series{{m.st.rtt, r.rtt[r.focus].points}}
	switch {
	case chartH < 5:
	case len(lines) == 0:
		out = append(out, m.st.chart(rtt, r.marks, msAxis, span, w, chartH))
	case loaded && chartH >= 12:
		out = append(out, m.st.chart(lines, r.marks, rateAxis, span, w, chartH-5),
			m.st.chart(rtt, nil, msAxis, span, w, 5))
	default:
		out = append(out, m.st.chart(lines, r.marks, rateAxis, span, w, chartH))
	}
	return strings.Join(out, "\n")
}

func (m model) readings(stage goclient.StagePlan) string {
	r := m.run
	var readings []string
	for _, dir := range stage.Directions {
		label := arrows[dir] + " "
		sample, sampled := r.rates[dir]
		value := m.st.value.Render(fmtRate(r.shown[dir]))
		switch {
		case !sampled || r.phase != goclient.PhaseMeasuring:
			value = m.st.muted.Render(missing)
		case sample.Unavailable:
			value = m.st.muted.Render(missing + " window restarting")
		}
		readings = append(readings, m.st.text.Render(label)+value)
	}
	if len(stage.Directions) == 0 || m.cfg.LoadedLatency {
		label := "Loaded latency "
		if len(stage.Directions) == 0 {
			label = "Idle latency "
		}
		value := m.st.muted.Render(missing)
		if sample, ok := r.latest[r.focus]; ok {
			value = m.st.value.Render(fmtMs(sample.RTT))
		}
		if streak := r.timeouts[r.focus]; streak > 0 {
			style := m.st.warn
			if streak >= 3 {
				style = m.st.err
			}
			value += style.Render(fmt.Sprintf("  probe timeout ×%d", streak))
		}
		readings = append(readings, m.st.text.Render(label)+value)
	}
	return strings.Join(readings, "   ")
}

func (m model) resultsView(w int) (string, string) {
	r := m.run
	latency := r.latencyPopulations()
	var idle *goclient.LatencyStats
	if population, ok := latency[goclient.StageLatency]; ok && population.HasMedian() {
		idle = &population.Latency
	}
	var throughput, latencyRows [][]string
	var notes, failures []string
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
			failures = append(failures, m.st.warn.Render(label+" stopped."))
		case err != nil:
			failures = append(failures, m.st.err.Render(label+": "+failureLabels[goclient.ReasonOf(err)]))
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
			for _, result := range r.results {
				if result.Stage == stage.Name {
					note(directionLabel(result), throughputFacts(result))
					failed(directionLabel(result), result.Err)
				}
			}
			rates := r.meanRates(stage.Name)
			measured = measured || rates != ""
			switch {
			case rates == "" && !r.live():
				rates = unmeasured(i)
			case r.stages[i].state == stagePartial:
				rates += "  " + m.st.warn.Render(stageStatusLabels[stagePartial])
			}
			if rates != "" {
				throughput = append(throughput, []string{compactStage(stage.Name), rates})
			}
		}
		population, ok := latency[stage.Name]
		switch {
		case ok:
			measured = true
			cells := latencyCells(population, idle)
			if stage.Name == goclient.StageLatency {
				cells[1] = ""
			}
			added = added || cells[1] != ""
			latencyRows = append(latencyRows, append([]string{compactPopulation(stage.Name)}, cells...))
			label := populationLabel(stage.Name)
			note(label, latencyFacts(population.Latency))
			if timing := population.Latency.ReflectorTiming; timing != nil {
				note(reflectorTimingFacts(timing))
			}
			failed(label, population.Err)
		case len(stage.Directions) == 0 && !r.live():
			latencyRows = append(latencyRows, []string{compactPopulation(stage.Name), unmeasured(i)})
		}
	}
	if !measured {
		return "", ""
	}
	if added {
		notes = append(notes, m.st.muted.Render("Added: loaded median minus idle median."))
	}
	var parts []string
	if len(throughput) > 0 {
		parts = append(parts, m.st.grid([]string{"Throughput", ""}, throughput, w))
	}
	if len(latencyRows) > 0 {
		headers := []string{"Latency", "Median", "Added", "P95", "Jitter", "Probe timeouts"}
		for i, row := range latencyRows {
			latencyRows[i] = append(row, make([]string, len(headers)-len(row))...)
		}
		parts = append(parts, m.st.grid(headers, latencyRows, w))
	}
	return strings.Join(append(parts, failures...), "\n"), strings.Join(notes, "\n")
}

func (m model) finalReport() string {
	if m.run == nil || m.running() {
		return ""
	}
	w, _ := m.size()
	lines := []string{"Graphite Meter · " + outcomeLabels[m.run.outcome]}
	if results, facts := m.resultsView(w); results != "" {
		lines = append(lines, results, facts)
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
