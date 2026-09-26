package main

import (
	"fmt"
	"slices"
	"strings"
	"time"

	"charm.land/bubbles/v2/key"
	"charm.land/bubbles/v2/viewport"
	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"
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
	if m.run != nil && m.run.details != nil {
		for _, s := range m.run.details.Servers {
			if s.Server.ID == id {
				return s.Server.Name
			}
		}
	}
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
		case time.Since(m.preparedRun.VerifiedAt) > goclient.PreparationFreshness:
			r.label = "Recheck needed"
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
	m.cfg.ServerIDs = m.readyServers()
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
	m.popup, m.serverRow, m.serverDraft = popupServers, 0, nil
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

func (m model) handleServerChooserKey(msg tea.KeyPressMsg) (tea.Model, tea.Cmd) {
	servers := m.preparedRun.Catalog.Servers
	switch {
	case key.Matches(msg, keys.discard):
		m.popup = popupNone
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
		m.cfg.ServerIDs = slices.Clone(m.serverDraft)
		m.popup = popupNone
		m.notice = "Checking the selected servers…"
		return m.reprepare()
	}
	return m, nil
}

func (m model) serverChooserView(w, h int) (string, string) {
	catalog := m.preparedRun.Catalog
	var lines []string
	capacity := max(2, (h-4)/2)
	start := min(max(m.serverRow-capacity/2, 0), max(0, len(catalog.Servers)-capacity))
	states := m.readiness()
	for i := start; i < min(len(catalog.Servers), start+capacity); i++ {
		server := catalog.Servers[i]
		label := serverLabel(server.Name, server.Location)
		if r := slices.IndexFunc(states, func(r readiness) bool { return r.server.ID == server.ID }); r >= 0 {
			label += " · " + states[r].label
		}
		line := m.st.checkbox(slices.Contains(m.serverDraft, server.ID)) + " " + label
		if i == m.serverRow {
			line = "› " + m.st.selected.Render(line)
		} else {
			line = "  " + line
		}
		lines = append(lines, line, "    "+m.st.muted.Render(server.URL))
	}
	return fmt.Sprintf("Test servers · %d selected", len(m.serverDraft)), fit(strings.Join(lines, "\n"), w)
}

func (m model) handleDetailsKey(msg tea.KeyPressMsg) (tea.Model, tea.Cmd) {
	switch {
	case key.Matches(msg, keys.close), key.Matches(msg, keys.details):
		m.popup = popupNone
		return m, nil
	}
	vp, cmd := m.detailsViewport().Update(msg)
	m.details = vp
	return m, cmd
}

func (m model) detailsViewport() viewport.Model {
	vp := m.details
	content := m.detailsView(m.popupWidth()-4, true)
	vp.SetWidth(m.popupWidth() - 4)
	vp.SetHeight(min(lipgloss.Height(content), m.layout().bodyH-2))
	vp.SetContent(content)
	return vp
}

func (m model) detailsView(w int, intervals bool) string {
	r := m.run
	if r == nil || r.details == nil {
		return m.st.muted.Render("Waiting for the first server report…")
	}
	details := r.details
	headers := []string{"Server"}
	var columns []goclient.Result
	for _, stage := range r.plan {
		for _, dir := range stage.Directions {
			columns = append(columns, goclient.Result{Stage: stage.Name, Direction: dir})
			headers = append(headers, directionLabel(columns[len(columns)-1]))
		}
	}
	row := func(name string, results []goclient.Result) []string {
		cells := []string{name}
		for _, column := range columns {
			value := missing
			for _, r := range results {
				if r.Stage == column.Stage && r.Direction == column.Direction && !r.Unavailable {
					value = fmtRate(r.MeanBps)
				}
			}
			cells = append(cells, value)
		}
		return cells
	}
	rows := [][]string{row("Combined", r.results)}
	latency := [][]string{}
	for _, server := range details.Servers {
		name := server.Server.Name
		if !slices.Contains(details.Participants, server.Server.ID) {
			name += " ✗"
		}
		rows = append(rows, row(name, server.Results))
		cells := []string{server.Server.Name}
		for _, stage := range r.plan {
			value := missing
			for _, result := range server.Results {
				if result.Stage == stage.Name && result.Direction == "" && result.Latency.Count > 0 {
					value = fmtMs(result.Latency.P50)
				}
			}
			cells = append(cells, value)
		}
		latency = append(latency, cells)
	}
	populations := []string{"Server"}
	for _, stage := range r.plan {
		populations = append(populations, compactPopulation(stage.Name))
	}
	lines := []string{m.st.heading.Render(m.outcomeNotice()), m.st.grid(headers, rows, w), "",
		m.st.heading.Render("Latency median by server"), m.st.grid(populations, latency, w)}
	if len(details.Failures) > 0 {
		lines = append(lines, "", m.st.heading.Render("Left the test"))
		for _, f := range details.Failures {
			lines = append(lines, fmt.Sprintf("%s · %s %s · at %s · %s",
				m.serverName(f.ServerID), compactStage(f.Stage), f.Scope, fmtClock(f.At), errorText(f.Err)))
		}
	}
	if intervals && details.Outcome != goclient.OutcomeRunning && len(details.Intervals) > 0 {
		lines = append(lines, "", m.st.heading.Render("Aggregation intervals"))
		for _, interval := range details.Intervals {
			state := "incomplete evidence"
			if interval.Complete && interval.Window != nil {
				state = "measured window"
			}
			names := make([]string, len(interval.Participants))
			for i, id := range interval.Participants {
				names[i] = m.serverName(id)
			}
			lines = append(lines, m.st.muted.Render(fmt.Sprintf("%s %.1f–%.1f s · %s · %s",
				compactStage(interval.Stage), interval.Start.Seconds(), interval.End.Seconds(),
				strings.Join(names, ", "), state)))
		}
		if details.OmittedIntervals > 0 {
			lines = append(lines, m.st.muted.Render(fmt.Sprintf(
				"%d older intervals omitted; byte totals retain the full run", details.OmittedIntervals)))
		}
	}
	return fit(strings.Join(lines, "\n"), w)
}

func (m model) outcomeNotice() string {
	details := m.run.details
	remaining, selected := len(details.Participants), len(details.Servers)
	switch {
	case selected == 1:
		return m.statusLabel()
	case m.run.outcome == goclient.OutcomeRunning && remaining < selected:
		return fmt.Sprintf("%d of %d servers remaining", remaining, selected)
	case m.run.outcome == goclient.OutcomeRunning:
		return fmt.Sprintf("%d servers combined", selected)
	case remaining < selected:
		return fmt.Sprintf("%s · %d of %d servers", outcomeLabels[m.run.outcome], remaining, selected)
	}
	return fmt.Sprintf("%s · %d servers combined", outcomeLabels[m.run.outcome], selected)
}
