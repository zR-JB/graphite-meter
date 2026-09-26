package main

import (
	"fmt"
	"slices"
	"strings"

	"github.com/charmbracelet/bubbles/key"
	tea "github.com/charmbracelet/bubbletea"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func (m model) canChooseServers() bool {
	return m.preparedRun != nil && len(m.preparedRun.Catalog.Servers) > 1
}

func (m model) multipleRunServers() bool {
	return m.run != nil && m.run.details != nil && len(m.run.details.Servers) > 1
}

func (m model) catalogServer(id string) (wire.ServerEntry, bool) {
	if m.preparedRun == nil {
		return wire.ServerEntry{}, false
	}
	i := slices.IndexFunc(m.preparedRun.Catalog.Servers, func(s wire.ServerEntry) bool { return s.ID == id })
	if i < 0 {
		return wire.ServerEntry{}, false
	}
	return m.preparedRun.Catalog.Servers[i], true
}

func (m model) serverName(id string) string {
	if server, ok := m.catalogServer(id); ok {
		return server.Name
	}
	return id
}

type readiness struct {
	server wire.ServerEntry
	label  string
	detail string
	ready  bool
}

func (m model) readiness() []readiness {
	if m.preparedRun == nil {
		return nil
	}
	out := make([]readiness, 0, len(m.preparedRun.Servers))
	for _, s := range m.preparedRun.Servers {
		r := readiness{server: s.Server, label: "Ready", ready: s.Err == nil && s.Connection != nil}
		switch {
		case m.prepare == prepareChecking:
			r.label, r.ready = "Checking…", false
		case isAuthRequired(s.Err):
			r.label = "Sign in"
		case !r.ready:
			r.label = "Unavailable"
			if s.Err != nil {
				r.detail = errorText(s.Err)
			}
		}
		out = append(out, r)
	}
	return out
}

func (m model) readyServers() []string {
	var ids []string
	for _, r := range m.readiness() {
		if r.ready {
			ids = append(ids, r.server.ID)
		}
	}
	return ids
}

func (m model) canUseAvailable() bool {
	ready := len(m.readyServers())
	return m.prepare != prepareChecking && ready > 0 && ready < len(m.readiness())
}

func (m model) selectedServerNames() string {
	if m.preparedRun == nil || len(m.preparedRun.Servers) == 0 {
		return missing
	}
	names := make([]string, len(m.preparedRun.Servers))
	for i, s := range m.preparedRun.Servers {
		names[i] = s.Server.Name
	}
	return strings.Join(names, ", ")
}

func (m model) readinessSummary() string {
	rows := m.readiness()
	ready := len(m.readyServers())
	switch {
	case len(rows) == 0:
		return ""
	case m.prepare == prepareChecking:
		return "checking"
	case ready == len(rows):
		return "ready"
	}
	return fmt.Sprintf("%d of %d ready", ready, len(rows))
}

func (m model) useAvailableServers() (tea.Model, tea.Cmd) {
	ids := m.readyServers()
	if err := m.controller.SelectServers(ids); err != nil {
		m.notice = err.Error()
		return m, nil
	}
	m.cfg.ServerIDs = ids
	m.notice = "Using the available servers."
	return m.reprepare()
}

func (m model) openServerChooser() (tea.Model, tea.Cmd) {
	switch {
	case m.prepare == prepareChecking:
		m.openChooser = true
		m.notice = "Test servers open when the path check finishes."
		return m, nil
	case m.preparedRun == nil:
		m.openChooser = true
		m.notice = "Loading servers…"
		return m.reprepare()
	case !m.canChooseServers():
		m.notice = "This catalogue offers one server."
		return m, nil
	}
	m.serverChooser, m.serverRow, m.serverDraft = true, 0, nil
	ids := m.cfg.ServerIDs
	if len(ids) == 0 {
		ids = m.preparedRun.Catalog.DefaultSelection
	}
	for _, server := range m.preparedRun.Catalog.Servers {
		if slices.Contains(ids, server.ID) {
			m.serverDraft = append(m.serverDraft, server.ID)
		}
	}
	m.notice = "Choose up to 4. Their speeds are combined."
	return m, nil
}

func (m model) handleServerChooserKey(msg tea.KeyMsg) (tea.Model, tea.Cmd) {
	servers := m.preparedRun.Catalog.Servers
	switch {
	case key.Matches(msg, keys.quit):
		m.close()
		return m, tea.Quit
	case key.Matches(msg, keys.discard):
		m.serverChooser = false
		m.notice = "Server selection unchanged."
	case key.Matches(msg, keys.rows):
		step := 1
		if reverse(msg) {
			step = -1
		}
		m.serverRow = min(max(m.serverRow+step, 0), len(servers)-1)
	case key.Matches(msg, keys.toggleServer):
		id := servers[m.serverRow].ID
		switch {
		case slices.Contains(m.serverDraft, id):
			m.serverDraft = slices.DeleteFunc(slices.Clone(m.serverDraft), func(v string) bool { return v == id })
		case len(m.serverDraft) < wire.MaxSelectedServers:
			m.serverDraft = append(slices.Clone(m.serverDraft), id)
		default:
			m.notice = "At most four servers share one test."
		}
	case key.Matches(msg, keys.apply):
		if err := m.controller.SelectServers(m.serverDraft); err != nil {
			m.notice = err.Error()
			return m, nil
		}
		m.cfg.ServerIDs = slices.Clone(m.serverDraft)
		m.serverChooser = false
		m.notice = "Checking the selected servers…"
		return m.reprepare()
	}
	return m, nil
}

func (m model) serverChooserView(w int) string {
	catalog := m.preparedRun.Catalog
	lines := []string{
		accentStyle.Render(fmt.Sprintf("Test servers · %d selected", len(m.serverDraft))),
		mutedStyle.Render("Choose up to 4. Their speeds are combined."),
		"",
	}
	capacity := max(3, min(12, (max(m.height, 20)-10)/2))
	start := min(max(m.serverRow-capacity/2, 0), max(0, len(catalog.Servers)-capacity))
	for i := start; i < min(len(catalog.Servers), start+capacity); i++ {
		server := catalog.Servers[i]
		label := server.Name
		if server.Location != "" {
			label += " · " + server.Location
		}
		if r := slices.IndexFunc(m.readiness(), func(r readiness) bool { return r.server.ID == server.ID }); r >= 0 {
			label += " · " + m.readiness()[r].label
		}
		line := checkbox(slices.Contains(m.serverDraft, server.ID)) + " " + label
		if i == m.serverRow {
			line = "› " + selectedStyle.Render(line)
		} else {
			line = "  " + line
		}
		lines = append(lines, line, "    "+mutedStyle.Render(server.URL))
	}
	return fitBlock(strings.Join(append(lines, "", m.notice), "\n"), w)
}

func (m model) handleDetailsKey(msg tea.KeyMsg) (tea.Model, tea.Cmd) {
	switch {
	case key.Matches(msg, keys.quit):
		m.close()
		return m, tea.Quit
	case key.Matches(msg, keys.setup), key.Matches(msg, keys.details):
		m.detailsOpen = false
	case key.Matches(msg, keys.scroll):
		step := 1
		if reverse(msg) {
			step = -1
		}
		lines := len(strings.Split(m.detailsView(m.innerWidth()), "\n"))
		m.detailsScroll = min(max(m.detailsScroll+step, 0), max(0, lines-m.detailsCapacity()))
	}
	return m, nil
}

func (m model) detailsCapacity() int { return max(3, m.height-9) }

func (m model) detailsOverlay(w int) string {
	lines := strings.Split(m.detailsView(w), "\n")
	start := min(m.detailsScroll, max(0, len(lines)-m.detailsCapacity()))
	return strings.Join(lines[start:min(len(lines), start+m.detailsCapacity())], "\n") +
		"\n\n" +
		mutedStyle.Render("↑/↓ scroll · esc closes details")
}

func (m model) detailsView(w int) string {
	r := m.run
	if r == nil || r.details == nil {
		return ""
	}
	details := r.details
	var columns []goclient.Result
	for _, stage := range r.plan {
		for _, dir := range stage.Directions {
			columns = append(columns, goclient.Result{Stage: stage.Name, Direction: dir})
		}
	}
	const nameWidth, cell = 16, 15
	header := pad("", nameWidth)
	for _, column := range columns {
		header += pad(directionLabel(column), cell)
	}
	lines := []string{accentStyle.Render("Details · " + m.outcomeNotice()), mutedStyle.Render(header)}
	row := func(name string, results []goclient.Result) string {
		line := pad(name, nameWidth)
		for _, column := range columns {
			value := missing
			for _, r := range results {
				if r.Stage == column.Stage && r.Direction == column.Direction && !r.Unavailable {
					value = fmtRate(r.MeanBps)
				}
			}
			line += pad(value, cell)
		}
		return line
	}
	lines = append(lines, valueStyle.Render(row("Combined", r.results)))
	for _, server := range details.Servers {
		name := server.Server.Name
		if !slices.Contains(details.Participants, server.Server.ID) {
			name += " ✗"
		}
		lines = append(lines, row(name, server.Results))
	}
	lines = append(lines, "", mutedStyle.Render("Latency median by server"))
	for _, server := range details.Servers {
		var parts []string
		for _, result := range server.Results {
			if result.Direction == "" && result.Latency.Count > 0 {
				parts = append(parts, populationLabel(result.Stage)+" "+fmtMs(result.Latency.P50))
			}
		}
		text := missing
		if len(parts) > 0 {
			text = strings.Join(parts, " · ")
		}
		lines = append(lines, pad(server.Server.Name, nameWidth)+text)
	}
	if len(details.Failures) > 0 {
		lines = append(lines, "", mutedStyle.Render("Left the test"))
		for _, f := range details.Failures {
			lines = append(lines, fmt.Sprintf("%s · %s %s · at %s · %s",
				r.serverName(f.ServerID), compactStage(f.Stage), f.Scope, fmtClock(f.At), errorText(f.Err)))
		}
	}
	if details.Outcome != goclient.OutcomeRunning && len(details.Intervals) > 0 {
		lines = append(lines, "", mutedStyle.Render("Aggregation intervals (debug)"))
		for _, interval := range details.Intervals {
			state := "incomplete evidence"
			if interval.Complete && interval.Window != nil {
				state = "measured window"
			}
			lines = append(lines, mutedStyle.Render(fmt.Sprintf("%s %.1f–%.1f s · %s · %s",
				compactStage(interval.Stage), interval.Start.Seconds(), interval.End.Seconds(),
				strings.Join(interval.Participants, ", "), state)))
		}
		if details.OmittedIntervals > 0 {
			lines = append(lines, mutedStyle.Render(fmt.Sprintf(
				"%d older intervals omitted; byte totals retain the full run", details.OmittedIntervals)))
		}
	}
	return fitBlock(strings.Join(lines, "\n"), w)
}

func (m model) outcomeNotice() string {
	details := m.run.details
	if details == nil {
		return ""
	}
	remaining, selected := len(details.Participants), len(details.Servers)
	switch {
	case m.run.outcome == goclient.OutcomeRunning && remaining < selected:
		return fmt.Sprintf("%d of %d servers remaining", remaining, selected)
	case m.run.outcome == goclient.OutcomeRunning:
		return fmt.Sprintf("%d servers combined", selected)
	case remaining < selected:
		return fmt.Sprintf("%s · %d of %d servers", outcomeLabels[m.run.outcome], remaining, selected)
	}
	return fmt.Sprintf("%s · %d servers combined", outcomeLabels[m.run.outcome], selected)
}
