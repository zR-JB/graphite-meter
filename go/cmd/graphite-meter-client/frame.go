package main

import (
	"fmt"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
)

// The frame: the top bar, the console, a sheet or card over it on a dimmed backdrop, and the bottom bar with a notice
// in its middle.

// smallest is the smallest terminal the interface draws in, in columns and rows.
var smallest = [2]int{60, 16}

// size is the frame's size, at least the smallest drawn.
func (m model) size() (int, int) { return max(m.width, smallest[0]), max(m.height, smallest[1]) }

// entry is one line of a sheet: a heading, a label and its value (pressable when it has a target), text, or a gap.
type entry struct {
	heading      string
	label, value line
	target       target
	item         bool
	text         line
}

func (m model) View() tea.View {
	w, h := max(m.width, 1), max(m.height, 1)
	if m.width == 0 {
		w, h = 120, 40
	}
	c := newCanvas(w, h, m.tiles)
	m.paint(c)
	m.frame.hits = c.hits
	v := tea.NewView(c.render())
	v.AltScreen = true
	v.MouseMode = tea.MouseModeAllMotion
	label, _ := m.status()
	v.WindowTitle = "Graphite Meter · " + label
	switch {
	case m.next != nil:
		v.ProgressBar = tea.NewProgressBar(tea.ProgressBarIndeterminate, 0)
	case m.running():
		v.ProgressBar = tea.NewProgressBar(tea.ProgressBarDefault, m.progress())
	}
	return v
}

// progress is the share of the planned stage time done; an incomplete stage adds nothing.
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

func (m model) paint(c *canvas) {
	area := c.area()
	if area.w < smallest[0] || area.h < smallest[1] {
		notice := fmt.Sprintf("Enlarge the terminal to at least %d×%d.", smallest[0], smallest[1])
		c.center(0, area.h/2, area.w, styled(notice, m.pal.muted))
		return
	}
	body := rect{0, 1, area.w, area.h - 2}
	m.topBar(c, rect{0, 0, area.w, 1})
	m.console(c, body)
	if m.overlay != overlayNone || m.auth != nil {
		// What opens over the console takes every press; a press beside it closes it.
		c.dim(body, m.pal.canvas, m.pal.muted.fg, 0.55)
		c.hits = c.hits[:0]
		c.hit(body, target{kind: targetBackdrop})
		switch {
		case m.auth != nil:
			m.signInCard(c, body)
		case m.overlay == overlaySettings:
			entries, focus := m.settingsEntries()
			m.sheet(c, body, "Settings", entries, focus, m.settingsFooter())
		case m.overlay == overlayDetails:
			m.sheet(c, body, "Details", m.detailsEntries(), -1, nil)
		case m.overlay == overlayHelp:
			m.sheet(c, body, "Keys", m.helpEntries(), -1, nil)
		case m.overlay == overlayChooser:
			entries, focus := m.chooserEntries()
			m.sheet(c, body, fmt.Sprintf("Test servers · %d selected", len(m.serverDraft)), entries, focus, nil)
		case m.overlay == overlayConnect:
			m.connectCard(c, body)
		}
	}
	m.bottomBar(c, rect{0, area.h - 1, area.w, 1})
}

// topBar is the brand and the server tested, its readiness at the right.
func (m model) topBar(c *canvas, area rect) {
	p := m.pal
	c.fill(area, p.surface)
	host := m.cfg.BaseURL
	if _, rest, ok := strings.Cut(host, "://"); ok {
		host = rest
	}
	brand := styled("◆", p.stage(goclient.StageLatency)).and(" Graphite Meter", p.heading).and("   ", style{}).
		and(host, p.muted)
	c.text(area.x+1, area.y, area.w-2, brand)
	dot, text, st := m.readinessLine()
	if m.hover == (target{kind: targetServer}) {
		st = p.text
	}
	readiness := dot.and(" ", style{}).and(text, st)
	width := readiness.width()
	x := area.right() - width - 1
	if x > area.x+brand.width()+2 {
		c.text(x, area.y, width, readiness)
		c.hit(rect{x, area.y, width, 1}, target{kind: targetServer})
	}
}

// readinessLine is the selected servers' readiness as the top bar shows it: a dot and its words.
func (m model) readinessLine() (line, string, style) {
	p := m.pal
	var names []string
	if m.preparedRun != nil {
		for _, s := range m.preparedRun.Servers {
			names = append(names, serverLabel(s.Server.Name, s.Server.Location))
		}
	}
	joined := strings.Join(names, ", ")
	switch {
	case m.cfg.BaseURL == "":
		return styled("○", p.muted), "No server", p.muted
	case m.prepare == prepareChecking && !m.running():
		return styled(m.spinner(), p.accent), "Checking", p.muted
	case m.prepare == prepareSignIn || m.auth != nil:
		return styled("●", p.warn), "Sign in", p.warn
	case m.prepare == prepareFailed && len(m.readyServers()) == 0:
		if joined == "" {
			joined = "Unreachable"
		}
		return styled("●", fgStyle(p.err.fg)), joined, p.text
	}
	tone := p.ok
	if len(m.readyServers()) < len(names) && !m.running() {
		tone = p.warn
	}
	return styled("●", tone), joined, p.text
}

// bottomBar is the run's state, time and bytes at the left, a notice in the middle, the keys at the right.
func (m model) bottomBar(c *canvas, area rect) {
	p := m.pal
	c.fill(area, p.surface)
	label, tone := m.status()
	status := styled(label, tone.withBold())
	if r := m.run; r != nil {
		elapsed := m.now.Sub(r.started)
		if !r.finished.IsZero() {
			elapsed = r.finished.Sub(r.started)
		}
		status = status.and("   "+fmtClock(elapsed), p.text).and("   "+fmtBytes(uint64(r.moved)), p.text)
		if left, ok := m.left(); ok {
			status = status.and("   "+fmtClock(left)+" left", p.muted)
		}
	}
	c.text(area.x+1, area.y, area.w-2, status)
	type capped struct {
		text line
		act  action
	}
	var caps []capped
	for _, b := range m.barKeys() {
		help := b.binding.Help()
		does := help.Desc
		if b.action == actStart && m.run != nil {
			does = "run again"
		}
		caps = append(caps, capped{m.capLine(help.Key, does, m.hover == target{kind: targetCap, action: b.action}), b.action})
	}
	widths := func() int {
		total := 0
		for _, cp := range caps {
			total += cp.text.width() + 2
		}
		return total
	}
	room := max(area.w-status.width()-6, 0)
	for len(caps) > 1 && widths() > room {
		caps = append(caps[:len(caps)-2], caps[len(caps)-1])
	}
	x := area.right() - widths() - 1
	// A notice takes the bar's middle while it lasts, the build otherwise.
	from, to := area.x+status.width()+4, x-2
	if m.toast != nil && to > from+10 {
		text := m.toast.text.fit(to - from - 4)
		width := text.width() + 4
		at := rect{from + (to-from-width)/2, area.y, width, 1}
		c.fill(at, p.raised)
		c.text(at.x+2, at.y, width-4, text)
	} else if version := styled("native "+goclient.Version, p.muted); to > from+version.width()+2 {
		c.text(x-version.width()-3, area.y, version.width(), version)
	}
	for _, cp := range caps {
		width := cp.text.width()
		c.text(x, area.y, width, cp.text)
		c.hit(rect{x, area.y, width, 1}, target{kind: targetCap, action: cp.act})
		x += width + 2
	}
}

// barKeys are the keys the bottom bar names for what is on top.
func (m model) barKeys() []binding {
	if m.edit != nil {
		return editKeys
	}
	if m.auth != nil {
		return signInKeys
	}
	var wanted []action
	switch m.overlay {
	case overlayNone, overlayConnect:
		wanted = []action{actStart, actSettings, actDetails, actHelp}
		if m.running() {
			wanted = []action{actStop, actDetails, actHelp}
		}
	case overlaySettings:
		wanted = []action{actMove, actAdjust, actToggle, actOpen, actClose}
	default:
		wanted = []action{actScroll, actMove, actSelect, actApply, actCancel, actClose}
	}
	var shown []binding
	for _, b := range m.table() {
		if b.binding.Help().Key == "" || !m.offers(b.action) {
			continue
		}
		for _, a := range wanted {
			if a == b.action && !containsAction(shown, a) {
				shown = append(shown, b)
			}
		}
	}
	return shown
}

func containsAction(bindings []binding, a action) bool {
	for _, b := range bindings {
		if b.action == a {
			return true
		}
	}
	return false
}

// status is the state and its tone: the outcome's, the measuring stage's, or how the server stands.
func (m model) status() (string, style) {
	p := m.pal
	if m.next != nil {
		return "Checking paths", p.muted
	}
	if r := m.run; r != nil {
		switch {
		case r.outcome != goclient.OutcomeRunning:
			return outcomeLabels[r.outcome], p.outcome(r.outcome)
		case m.stopping:
			return "Stopping", p.muted
		case r.stage == "" || r.phase == goclient.PhasePreparing:
			return "Checking paths", p.muted
		case r.phase == goclient.PhaseWarmup:
			return "Warmup", p.muted
		}
		return stageLabels[r.stage], p.stage(r.stage)
	}
	switch {
	case m.prepare == prepareNoServer:
		return notStarted, p.muted
	case m.auth != nil && m.auth.opened:
		return checkingSignIn, p.muted
	case m.prepare == prepareSignIn || m.auth != nil:
		return pathLabels[pathSignIn], p.warn
	case m.prepare == prepareFailed && len(m.readyServers()) == 0:
		return startFailed, p.warn
	}
	return notStarted, p.muted
}

// sheet draws a sheet titled title at the right of area: its entries from the scroll on, the focused one lit, and
// footer at its foot.
func (m model) sheet(c *canvas, area rect, title string, entries []entry, focus int, footer []line) {
	p := m.pal
	width := area.w
	if area.w >= 96 {
		width = min(max(area.w*2/5, 56), 80)
	}
	s := rect{area.right() - width, area.y, width, area.h}
	c.clear(s)
	c.fill(s, p.surface)
	c.hit(s, target{kind: targetSheet})
	inner := max(width-4, 0)
	c.text(s.x+2, s.y+1, inner, styled(title, p.heading))
	closeCap := m.capLine("esc", "close", m.hover == target{kind: targetCap, action: actClose})
	cw := closeCap.width()
	c.text(s.right()-cw-2, s.y+1, cw, closeCap)
	c.hit(rect{s.right() - cw - 2, s.y + 1, cw, 1}, target{kind: targetCap, action: actClose})
	// Every entry as the lines it takes, and where the focused one starts.
	type sheetLine struct {
		text   line
		index  int
		target target
	}
	var lines []sheetLine
	focused := -1
	for i, e := range entries {
		switch {
		case e.heading != "":
			if len(lines) > 0 {
				lines = append(lines, sheetLine{index: -1})
			}
			lines = append(lines, sheetLine{text: styled(e.heading, p.heading), index: -1})
		case e.item:
			if i == focus {
				focused = len(lines)
			}
			gap := max(inner-e.label.width()-e.value.width(), 2)
			row := e.label.and(strings.Repeat(" ", gap), style{}).with(e.value)
			lines = append(lines, sheetLine{text: row.fit(inner), index: i, target: e.target})
		case e.text != nil:
			st := p.muted
			if len(e.text) > 0 {
				st = e.text[0].st
			}
			for _, part := range wrapWords(e.text.text(), inner) {
				lines = append(lines, sheetLine{text: styled(part, st), index: -1})
			}
		default:
			lines = append(lines, sheetLine{index: -1})
		}
	}
	var foot []line
	for _, l := range footer {
		for _, part := range wrapWords(l.text(), inner) {
			foot = append(foot, styled(part, p.muted))
		}
	}
	top := s.y + 3
	bottom := s.bottom() - len(foot) - 1
	if len(foot) > 0 {
		bottom--
	}
	shown := max(bottom-top, 0)
	scroll := m.scroll
	if focused >= 0 {
		scroll = min(max(scroll, focused+1-shown), focused)
	}
	maxScroll := max(len(lines)-shown, 0)
	scroll = min(max(scroll, 0), maxScroll)
	*m.frame = frameCache{scroll: scroll, maxScroll: maxScroll, rows: shown}
	for offset, l := range lines[scroll:min(scroll+shown, len(lines))] {
		y := top + offset
		row := rect{s.x + 1, y, width - 2, 1}
		if l.index >= 0 {
			lit := l.index == focus
			hovered := l.target.kind != noTarget && m.hover == l.target
			switch {
			case lit:
				c.fill(row, p.selected)
				c.text(s.x+1, y, 1, styled("▌", p.accent))
			case hovered:
				c.fill(row, p.hover)
			}
			if l.target.kind != noTarget {
				c.hit(row, l.target)
			}
		}
		c.text(s.x+2, y, inner, l.text)
	}
	if more := len(lines) > scroll+shown; more || scroll > 0 {
		hint := "↓ more"
		switch {
		case scroll > 0 && more:
			hint = "↑↓ more"
		case scroll > 0:
			hint = "↑ more"
		}
		c.right(s.x+2, bottom, inner, styled(hint, p.muted))
	}
	for i, l := range foot {
		c.text(s.x+2, bottom+1+i, inner, l)
	}
}

// wrapWords fills text's words into lines of at most w cells; a longer word takes a line of its own.
func wrapWords(text string, w int) []string {
	var lines []string
	for _, word := range strings.Fields(text) {
		if n := len(lines); n > 0 && ansi.StringWidth(lines[n-1])+1+ansi.StringWidth(word) <= w {
			lines[n-1] += " " + word
			continue
		}
		lines = append(lines, word)
	}
	return lines
}

// cardOver fills a card centred over the console, height rows tall at most, and returns its inner area.
func (m model) cardOver(c *canvas, area rect, height int) rect {
	width, height := min(max(area.w-4, 0), 76), min(height, area.h)
	card := rect{area.x + (area.w-width)/2, area.y + (area.h-height)/3, width, height}
	c.clear(card)
	c.fill(card, m.pal.surface)
	c.hit(card, target{kind: targetSheet})
	return rect{card.x + 3, card.y + 1, max(width-6, 0), max(height-2, 0)}
}

// connectCard asks for the server's address: the field, and how its check stands.
func (m model) connectCard(c *canvas, area rect) {
	p := m.pal
	inner := m.cardOver(c, area, 13)
	c.text(inner.x, inner.y, inner.w, styled("Connect to a server", p.heading))
	c.text(inner.x, inner.y+2, inner.w, styled("Enter the address of your Graphite Meter server, or paste it.", p.muted))
	field := rect{inner.x, inner.y + 4, inner.w, 1}
	c.fill(field, p.raised)
	if m.edit != nil {
		m.edit.input.SetWidth(max(field.w-3, 1))
		c.text(field.x+1, field.y, field.w-2, m.editLine(field.w-2))
	}
	host := m.cfg.BaseURL
	if _, rest, ok := strings.Cut(host, "://"); ok {
		host = rest
	}
	var said, next line
	switch {
	case m.edit != nil && m.edit.err != "":
		said = styled(m.edit.err, fgStyle(p.err.fg))
	case m.prepare == prepareChecking && host != "":
		said = styled(m.spinner(), p.accent).and(" Checking "+host+"…", p.muted)
	case m.prepare == prepareFailed && len(m.readyServers()) == 0:
		text := m.prepareErr
		if text == "" {
			text = m.firstFailure()
		}
		said = styled(host+": "+text, p.warn)
		if text == unreachable {
			said = styled("Couldn't reach "+host+".", p.warn)
		}
		next = styled("Check the address and that the server is running; enter tries again.", p.muted)
	}
	c.text(inner.x, inner.y+6, inner.w, said.fit(inner.w))
	c.text(inner.x, inner.y+7, inner.w, next.fit(inner.w))
	// Remembering is the user's choice: the switch, lit under the pointer, and its key.
	row := rect{inner.x, inner.y + 9, inner.w, 1}
	if m.hover == (target{kind: targetRemember}) {
		c.fill(rect{row.x - 1, row.y, row.w + 2, 1}, p.hover)
	}
	c.text(row.x, row.y, row.w, styled("Remember this server  ", p.text).with(m.switchMark(m.remembering)))
	c.right(row.x, row.y, row.w, styled("tab", p.muted))
	c.hit(row, target{kind: targetRemember})
	keys := styled("enter", p.text).and(" connects    ", p.muted).and("esc", p.text).and(" closes", p.muted)
	c.text(inner.x, inner.y+10, inner.w, keys)
}

// firstFailure is the first selected server's failure, or that none answered.
func (m model) firstFailure() string {
	if m.preparedRun != nil {
		for _, s := range m.preparedRun.Servers {
			if s.Err != nil {
				return errorText(s.Err)
			}
		}
	}
	return "No test server answered"
}

// signInCard is the code to match, the time waited and left, and the page's address as a link.
func (m model) signInCard(c *canvas, area rect) {
	p := m.pal
	issuer := m.serverName(m.challengedServer())
	if issuer == "" {
		issuer = m.cfg.BaseURL
	}
	link := m.auth.pending.BrowserURL
	width := max(min(area.w-10, 70), 1)
	var wrapped []string
	for runes := []rune(link); len(runes) > 0; {
		n := min(len(runes), width)
		wrapped, runes = append(wrapped, string(runes[:n])), runes[n:]
	}
	inner := m.cardOver(c, area, 12+len(wrapped))
	c.text(inner.x, inner.y, inner.w, styled("Sign in to "+issuer, p.heading))
	status := styled("Open the sign-in page and check that it shows this code.", p.muted)
	if m.auth.opened {
		status = styled(m.spinner(), p.accent).and(" Waiting for approval…", p.muted)
	}
	c.text(inner.x, inner.y+2, inner.w, status)
	code := rect{inner.x, inner.y + 4, ansi.StringWidth(m.auth.pending.Code) + 6, 3}
	c.fill(code, p.raised)
	c.text(code.x+3, code.y+1, code.w-6, styled(m.auth.pending.Code, p.value))
	waited := m.now.Sub(m.auth.since)
	times := fmt.Sprintf("waited %s · expires in %s", fmtClock(waited),
		fmtSetting(max(goclient.AuthorizationTimeout-waited, 0).Round(time.Second)))
	c.text(inner.x, inner.y+8, inner.w, styled(times, p.muted))
	for i, part := range wrapped {
		y := inner.y + 10 + i
		if y >= inner.bottom() {
			break
		}
		c.text(inner.x, y, inner.w, styled(part, p.accent))
		c.hit(rect{inner.x, y, ansi.StringWidth(part), 1}, target{kind: targetLink})
	}
}

// helpEntries are every key, grouped by where it works.
func (m model) helpEntries() []entry {
	groups := []struct {
		title string
		table []binding
	}{{"Console", consoleKeys}, {"Settings", settingsKeys}, {"Details and keys", detailsKeys},
		{"Test servers", chooserKeys}, {"Typing", editKeys}}
	var entries []entry
	for _, g := range groups {
		entries = append(entries, entry{heading: g.title})
		for _, b := range g.table {
			help := b.binding.Help()
			if help.Key == "" {
				continue
			}
			does := strings.ToUpper(help.Desc[:1]) + help.Desc[1:]
			entries = append(entries, entry{item: true, label: styled(does, m.pal.text), value: styled(help.Key, m.pal.value)})
		}
	}
	return append(entries, entry{}, entry{text: styled("Click the run key, a stage's chip or a key below to press it. "+
		"Hold shift to select text with the mouse.", m.pal.muted)})
}

// detailsEntries are the servers, paths, results and build, as label and value under their headings.
func (m model) detailsEntries() []entry {
	p := m.pal
	item := func(label, value string) entry {
		return entry{item: true, label: styled(label, p.muted), value: styled(value, p.text)}
	}
	var entries []entry
	if m.preparedRun != nil {
		for _, s := range m.preparedRun.Servers {
			heading := "Tested server"
			if len(m.preparedRun.Servers) > 1 {
				heading = s.Server.Name
			}
			entries = append(entries, entry{heading: heading}, item("Name", s.Server.Name), item("Address", s.Server.URL))
			if s.Server.Location != "" {
				entries = append(entries, item("Location", s.Server.Location))
			}
			if c := s.Connection; c != nil {
				t := c.ThroughputTarget
				entries = append(entries, item("Throughput path", connectionSummary(t.Transport, t.Protocol, t.TLS(), false)),
					item("Streams", streamsLabel(m.cfg.TransferStreams, t.Protocol, t.Transport)))
				latency := missing
				if l := c.LatencyTarget; l != nil {
					latency = connectionSummary(l.Transport, l.Protocol, l.TLS(), true)
				}
				entries = append(entries, item("Latency path", latency))
				if c.WarmRTT > 0 {
					entries = append(entries, item("Pre-test latency", fmtMs(c.WarmRTT)))
				}
				if c.IPVersion != 0 {
					entries = append(entries, item("IP family", fmt.Sprintf("IPv%d", c.IPVersion)))
				}
			} else if s.Err != nil {
				entries = append(entries, entry{text: styled(errorText(s.Err), p.warn)})
			}
			if m.run != nil && m.run.details != nil {
				for _, summary := range m.run.details.Servers {
					if summary.Server.ID != s.Server.ID {
						continue
					}
					for _, stage := range m.run.plan {
						rate := 0.0
						for _, result := range summary.Results {
							if result.Stage == stage.Name && result.Direction != "" && !result.Unavailable {
								rate += result.MeanBps
							}
						}
						if rate > 0 {
							entries = append(entries, item(stageIcons[stage.Name]+" "+stageLabels[stage.Name], fmtRate(rate)))
						}
					}
				}
			}
		}
	}
	if len(entries) == 0 {
		address := m.cfg.BaseURL
		if address == "" {
			address = missing
		}
		entries = append(entries, entry{heading: "Tested server"}, item("Address", address),
			entry{text: styled("Its test servers show here once it answers.", p.muted)})
	}
	if m.run != nil && m.run.details != nil && len(m.run.details.Failures) > 0 {
		entries = append(entries, entry{heading: "Issues"})
		for _, f := range m.run.details.Failures {
			entries = append(entries, item(stageLabels[f.Stage]+" at "+fmtClock(f.At),
				m.serverName(f.ServerID)+": "+failureLabels[f.Reason]))
		}
	}
	if m.run != nil {
		var rows []entry
		for _, stage := range m.run.plan {
			stats := m.population(stage.Name)
			if stats == nil || stats.Count == 0 {
				continue
			}
			rows = append(rows, item(compactPopulation(stage.Name), fmt.Sprintf("median %s  p95 %s  %s replies",
				fmtMs(stats.P50), fmtMs(stats.P95), fmtCount(stats.Count))))
		}
		if len(rows) > 0 {
			entries = append(append(entries, entry{heading: "Latency to " + m.serverName(m.run.latencyServer())}), rows...)
		}
	}
	return append(entries, entry{heading: "Build"}, item("Client", "native "+goclient.Version))
}
