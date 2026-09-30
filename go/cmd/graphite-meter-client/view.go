package main

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"math"
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
	v.MouseMode = tea.MouseModeCellMotion
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
		body = m.st.panel(title, content, inner, 0) + "\n\n" + m.signInLink(inner)
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
	start := min(m.body.YOffset(), max(len(f.body)-f.bodyH, 0))
	lines := make([]string, f.bodyH)
	copy(lines, f.body[start:min(start+f.bodyH, len(f.body))])
	content := f.top + "\n" + strings.Join(lines, "\n") + "\n" + f.footer
	w, _ := m.size()
	var b strings.Builder
	b.Grow(len(content) + len(lines)*2)
	for line := range strings.SplitSeq(content, "\n") {
		b.WriteString(" " + pad(line, w-2) + " \n")
	}
	return strings.TrimSuffix(b.String(), "\n")
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
	label, pill := m.statusLabel(), m.st.pill
	switch r := m.run; {
	case r != nil && !m.running():
		pill = m.st.outcome[r.outcome]
	case r != nil && label == stageLabels[r.stage]:
		pill = pill.Background(m.st.stage[r.stage].GetForeground())
	}
	right := pill.Render(label)
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
			input := m.edit.input
			input.SetWidth(max(w-labelWidth-4, 1))
			value = input.View()
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
			pathStale: m.st.warn.Render("○"), pathFailed: m.st.err.Render("✗"),
			pathSignIn: m.st.warn.Render("○")}[r.state]
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
	status := m.st.accent.Render("Open the sign-in page below")
	if m.auth.opened {
		status = m.spin.View() + " " + m.st.accent.Render("Waiting for approval…")
	}
	waited := m.now.Sub(m.auth.since)
	code := lipgloss.NewStyle().Border(lipgloss.RoundedBorder()).BorderForeground(m.st.border.GetForeground()).
		Padding(0, 1).Render(m.st.value.Render(m.auth.pending.Code))
	lines := []string{
		status,
		lipgloss.JoinHorizontal(lipgloss.Center, m.st.text.Render("Match this code "), code),
		m.st.muted.Render(fmt.Sprintf("waited %s · expires in %s", fmtClock(waited),
			fmtSetting((goclient.AuthorizationTimeout - waited).Round(time.Second)))),
	}
	return "Sign in to " + issuer, strings.Join(lines, "\n")
}

func (m model) signInLink(w int) string {
	link := m.auth.pending.BrowserURL
	lines := strings.Split(ansi.Hardwrap(link, w, false), "\n")
	for i, line := range lines {
		lines[i] = m.st.accent.Hyperlink(link).Render(line)
	}
	return strings.Join(lines, "\n")
}

func (m model) runView(w, h int) string {
	results := ""
	if !m.run.live() {
		results = m.resultsView(w-4, "Latency").view()
	}
	if results == "" {
		return m.stageView(w, h)
	}
	title := "Results"
	if m.multipleRunServers() {
		title += " · latency to " + m.serverName(m.run.latencyServer())
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
			if l := server.LatencyTarget; server.Server.ID == r.latencyServer() && l != nil {
				latency = connectionSummary(l.Transport, l.Protocol, l.TLS(), true)
			}
		}
		servers := strings.Join(names, ", ")
		if len(names) > 1 {
			servers += " (all servers)"
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
		hue := m.st.stage[s.name]
		name := hue.Render(pad(stageLabels[s.name], 14))
		elapsed := max(m.now.Sub(s.since), 0)
		switch s.state {
		case stagePreparing:
			lines = append(lines, name+m.spin.View()+m.st.muted.Render(" checking paths"))
		case stageWarmup:
			lines = append(lines, name+m.spin.View()+m.st.muted.Render(" warmup ")+m.st.value.Render(fmtClock(elapsed)))
		case stageMeasuring:
			clock := m.st.value.Render(fmtClock(min(elapsed, s.duration)))
			clock += m.st.muted.Render(" / " + fmtSetting(s.duration))
			lines = append(lines, name+m.st.bar(hue, elapsed.Seconds(), s.duration.Seconds(), barW)+"  "+clock)
		case stageDone:
			value := m.headline(s.name)
			if value == "" {
				value = m.st.muted.Render(fmtSetting(s.duration))
			}
			lines = append(lines, name+m.st.ok.Render("✓ ")+value)
		case stagePartial:
			value := strings.TrimSpace(m.headline(s.name) + " " + m.st.muted.Render(stageStatusLabels[s.state]))
			lines = append(lines, name+m.st.warn.Render("! ")+value)
		case stageFailed:
			lines = append(lines, name+m.st.err.Render("✗ ")+m.st.muted.Render(stageStatusLabels[s.state]))
		case stageStopped:
			lines = append(lines, name+m.st.muted.Render("○ "+stageStatusLabels[s.state]))
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
	var out []string
	if r.live() {
		out = append(out, wrapParts(strings.Split(m.readings(stage), "   "), w)...)
		if stage.Name == goclient.StageBidirectional {
			out = append(out, m.st.muted.Render("↓ solid · ↑ dashed"))
		}
	}
	if m.multipleRunServers() {
		out = append(out, m.st.muted.Render("Latency to "+m.serverName(r.latencyServer())+" · l switches server"))
	}
	chartH := h - len(out)
	span := max(r.span, math.Ceil(max(m.now.Sub(r.started).Seconds(), 0)/10)*10, 1)
	if !r.live() && !r.finished.IsZero() {
		span = max(math.Ceil(r.finished.Sub(r.started).Seconds()), 1)
	}
	key := chartKey{marks: len(r.marks), w: w, span: span, dark: m.st.dark, stage: r.stage, finished: !r.live()}
	latencyChart := func(height int) string {
		k := key
		k.h, k.server = height, r.latencyServer()
		k.versions[0] = r.rtt[k.server].version
		return r.charts[1].render(k, func() string {
			return m.st.chart(m.st.stageSeries(r.rtt[k.server].points, r.marks), r.marks, msAxis, span, w, height)
		})
	}
	rateChart := func(height int) string {
		k := key
		k.h = height
		for i, dir := range dirs {
			k.versions[i] = r.history[dir].version
			k.directions[i] = dir
		}
		return r.charts[0].render(k, func() string {
			var lines []series
			for _, dir := range dirs {
				lines = append(lines, m.st.stageSeries(r.history[dir].points, r.marks, dir)...)
			}
			return m.st.chart(lines, r.marks, rateAxis, span, w, height)
		})
	}
	switch {
	case chartH < 5:
	case len(dirs) == 0:
		out = append(out, latencyChart(chartH))
	case loaded && chartH >= 12:
		out = append(out, rateChart(chartH-5), latencyChart(5))
	default:
		out = append(out, rateChart(chartH))
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
		readings = append(readings, m.st.stage[stage.Name].Render(label)+value)
	}
	if len(stage.Directions) == 0 || m.cfg.LoadedLatency {
		label := "Loaded latency "
		if len(stage.Directions) == 0 {
			label = "Idle latency "
		}
		value := m.st.muted.Render(missing)
		if sample, ok := r.latest[r.latencyServer()]; ok {
			value = m.st.value.Render(fmtMs(sample.RTT))
		}
		if streak := r.timeouts[r.latencyServer()]; streak > 0 {
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

type results struct {
	throughput, latency string
	failures, notes     []string
	added               bool
}

func (r results) view() string {
	parts := append([]string{r.throughput, r.latency}, r.failures...)
	return strings.Join(slices.DeleteFunc(parts, func(part string) bool { return part == "" }), "\n")
}

func (m model) note(label string, facts []string, w int) []string {
	lines := []string{label + ":"}
	if first := label + ": " + facts[0]; lipgloss.Width(first) <= w-2 {
		lines, facts = nil, append([]string{first}, facts[1:]...)
	}
	for _, line := range wrapParts(facts, w-2) {
		if len(lines) > 0 {
			line = "  " + line
		}
		lines = append(lines, line)
	}
	for i, line := range lines {
		lines[i] = m.st.muted.Render(line)
	}
	return lines
}

func (r *runState) unmeasured(i int) string {
	return cmp.Or(stageStatusLabels[r.stages[i].state], missing)
}

func (r *runState) failureReason(stage goclient.Stage, err error) goclient.FailureReason {
	if r.details != nil && errors.Is(err, context.Canceled) {
		failures := r.details.Failures
		i := slices.IndexFunc(failures, func(f goclient.ServerFailure) bool {
			return f.Stage == stage && f.ServerID == r.latencyServer()
		})
		if i < 0 {
			i = slices.IndexFunc(failures, func(f goclient.ServerFailure) bool { return f.Stage == stage })
		}
		if i >= 0 {
			return failures[i].Reason
		}
	}
	return goclient.ReasonOf(err)
}

func (m model) resultsView(w int, latencyHeading string) results {
	r := m.run
	latency := r.latencyPopulations()
	var idle *goclient.LatencyStats
	if population, ok := latency[goclient.StageLatency]; ok && population.HasMedian() {
		idle = &population.Latency
	}
	var out results
	var throughput, latencyRows [][]string
	failed := func(stage goclient.Stage, label string, err error) {
		switch {
		case err == nil:
		case errors.Is(err, context.Canceled) && r.outcome == goclient.OutcomeStopped:
			out.failures = append(out.failures, m.st.warn.Render(label+" stopped."))
		default:
			out.failures = append(out.failures, m.st.err.Render(label+": "+failureLabels[r.failureReason(stage, err)]))
		}
	}
	measured := false
	for i, stage := range r.plan {
		hue := m.st.stage[stage.Name]
		if len(stage.Directions) > 0 {
			for _, result := range r.results {
				if result.Stage != stage.Name {
					continue
				}
				if !result.Unavailable || result.TotalBytes > 0 {
					out.notes = append(out.notes, m.note(directionLabel(result), throughputFacts(result, false), w)...)
				}
				failed(stage.Name, directionLabel(result), result.Err)
			}
			rates := r.meanRates(stage.Name)
			measured = measured || rates != ""
			switch {
			case rates == "" && !r.live():
				rates = r.unmeasured(i)
			case r.stages[i].state == stagePartial:
				rates += "  " + m.st.warn.Render(stageStatusLabels[stagePartial])
			}
			if rates != "" {
				throughput = append(throughput, []string{hue.Render(compactStage(stage.Name)), rates})
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
			out.added = out.added || cells[1] != ""
			latencyRows = append(latencyRows, append([]string{hue.Render(compactPopulation(stage.Name))}, cells...))
			label := populationLabel(stage.Name)
			out.notes = append(out.notes, m.note(label, latencyFacts(population.Latency), w)...)
			if timing := population.Latency.ReflectorTiming; timing != nil {
				label, facts := reflectorTimingFacts(timing)
				out.notes = append(out.notes, m.note(label, facts, w)...)
			}
			failed(stage.Name, label, population.Err)
		case len(stage.Directions) == 0 && !r.live():
			latencyRows = append(latencyRows, []string{hue.Render(compactPopulation(stage.Name)), r.unmeasured(i)})
		}
	}
	if !measured {
		return results{}
	}
	if out.added {
		out.notes = append(out.notes, m.st.muted.Render(addedNote))
	}
	scope := ""
	if m.multipleRunServers() {
		scope = "All servers"
	}
	if len(throughput) > 0 {
		out.throughput = m.st.grid([]string{"Throughput", scope}, throughput, w)
	}
	if len(latencyRows) > 0 {
		headers := []string{latencyHeading, "Median", "Added", "P95", "Jitter", "Probe timeouts"}
		for i, row := range latencyRows {
			latencyRows[i] = append(row, make([]string, len(headers)-len(row))...)
			if !out.added {
				latencyRows[i] = slices.Delete(latencyRows[i], 2, 3)
			}
		}
		if !out.added {
			headers = slices.Delete(headers, 2, 3)
		}
		out.latency = m.st.grid(headers, latencyRows, w)
	}
	return out
}

func (m model) finalReport() string {
	if m.run == nil || m.running() {
		return ""
	}
	run := *m.run
	run.pick = ""
	m.run = &run
	w, _ := m.size()
	blocks := []string{m.reportHeader()}
	heading := "Latency"
	if m.multipleRunServers() {
		heading += " to " + m.serverName(m.run.latencyServer())
	}
	if results := m.resultsView(w, heading); results.view() != "" {
		blocks = append(blocks, m.throughputReport(w), results.latency, strings.Join(results.failures, "\n"),
			m.reportNotes(w, results.added))
	}
	if m.multipleRunServers() {
		blocks = append(blocks, m.detailsView(w, false))
	}
	if m.run.err != nil {
		blocks = append(blocks, m.st.err.Render(errorText(m.run.err)))
	}
	blocks = slices.DeleteFunc(blocks, func(block string) bool { return block == "" })
	report := strings.Split(strings.Join(blocks, "\n\n"), "\n")
	for i, line := range report {
		report[i] = strings.TrimRight(line, " ")
	}
	return strings.Join(report, "\n")
}

func (m model) reportHeader() string {
	r := m.run
	var facts []string
	switch {
	case r.details == nil:
	case len(r.details.Servers) == 1:
		facts = append(facts, r.details.Servers[0].Server.Name)
	default:
		facts = append(facts, fmt.Sprintf("%d servers", len(r.details.Servers)))
	}
	if !r.finished.IsZero() {
		facts = append(facts, fmtClock(r.finished.Sub(r.started)))
	}
	var total uint64
	for _, result := range r.results {
		total += result.TotalBytes
	}
	if total > 0 {
		facts = append(facts, fmtBytes(total))
	}
	tone := lipgloss.NewStyle().Bold(true).Foreground(m.st.outcome[r.outcome].GetBackground())
	header := m.st.heading.Render("Graphite Meter") + "  " + tone.Render(outcomeLabels[r.outcome])
	if len(facts) > 0 {
		header += "  " + m.st.muted.Render(strings.Join(facts, " · "))
	}
	return header
}

func (m model) throughputReport(w int) string {
	r := m.run
	type row struct {
		label, value, status string
		facts                []string
	}
	var rows []row
	labelW, valueW := 0, 0
	for i, stage := range r.plan {
		style := m.st.stage[stage.Name]
		for _, dir := range stage.Directions {
			row := row{label: style.Render(arrows[dir]) + " " + m.st.text.Render(stageLabels[stage.Name])}
			at := slices.IndexFunc(r.results, func(result goclient.Result) bool {
				return result.Stage == stage.Name && result.Direction == dir
			})
			switch {
			case at < 0:
				row.value = m.st.muted.Render(r.unmeasured(i))
			case r.results[at].Unavailable:
				row.value = m.st.muted.Render(missing)
			default:
				row.value = style.Bold(true).Render(fmtRate(r.results[at].MeanBps))
				row.facts = throughputFacts(r.results[at], true)
			}
			if r.stages[i].state == stagePartial {
				row.status = stageStatusLabels[stagePartial]
				row.facts = append([]string{row.status}, row.facts...)
			}
			labelW, valueW = max(labelW, lipgloss.Width(row.label)), max(valueW, lipgloss.Width(row.value))
			rows = append(rows, row)
		}
	}
	indent := labelW + valueW + 2
	var lines []string
	for _, row := range rows {
		line := pad(row.label, labelW) + "  " + pad(row.value, valueW)
		for i, facts := range wrapParts(row.facts, max(w-indent-3, 20)) {
			switch {
			case i > 0:
				line = strings.Repeat(" ", indent) + "   " + m.st.muted.Render(facts)
			case row.status != "":
				line += "   " + m.st.warn.Render(row.status) + m.st.muted.Render(strings.TrimPrefix(facts, row.status))
			case facts != "":
				line += "   " + m.st.muted.Render(facts)
			}
			lines = append(lines, line)
		}
	}
	return strings.Join(lines, "\n")
}

func (m model) reportNotes(w int, added bool) string {
	var notes, timing []string
	populations := m.run.latencyPopulations()
	for _, stage := range m.run.plan {
		population, ok := populations[stage.Name]
		if !ok {
			continue
		}
		s := population.Latency
		if population.Err != nil || s.Timeouts > 0 || s.Unresolved > 0 || s.SendFailures > 0 {
			notes = append(notes, m.note(populationLabel(stage.Name), latencyFacts(s), w)...)
		}
		if t := s.ReflectorTiming; t != nil {
			part := compactPopulation(stage.Name) + " " + fmtMs(t.MeanHandling) + " of " + fmtMs(t.MeanRawRTT)
			if t.Count != s.Count {
				part += " (" + fmtCount(t.Count) + " pairs)"
			}
			timing = append(timing, part)
		}
	}
	if len(timing) > 0 {
		notes = append(notes, m.note("Server handling of the mean round trip", timing, w)...)
	}
	if added {
		notes = append(notes, m.st.muted.Render(addedNote))
	}
	return strings.Join(notes, "\n")
}
