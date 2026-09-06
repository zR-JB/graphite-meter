package main

import (
	"errors"
	"fmt"
	"slices"
	"strings"

	"github.com/charmbracelet/bubbles/key"
	tea "github.com/charmbracelet/bubbletea"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func (m model) multipleServers() bool { return len(m.cfg.ServerIDs) > 1 }
func (m model) openServerChooser() (tea.Model, tea.Cmd) {
	if m.mode == modeRun && !m.complete {
		return m, nil
	}
	if m.prepareStatus == "checking" {
		m.chooseAfterPrepare = true
		return m, nil
	}
	if m.preparedRun == nil || len(m.preparedRun.Catalog.Servers) == 0 {
		m.chooseAfterPrepare = true
		return m.reprepare(nil)
	}
	m.serverChooser = true
	m.serverRow = 0
	m.serverDraft = nil
	ids := m.cfg.ServerIDs
	if len(ids) == 0 {
		ids = m.preparedRun.Catalog.DefaultSelection
	}
	for _, server := range m.preparedRun.Catalog.Servers {
		if slices.Contains(ids, server.ID) {
			m.serverDraft = append(m.serverDraft, server.ID)
		}
	}
	m.notice = "Select one to four servers. Enter applies; Escape cancels."
	return m, nil
}
func (m model) handleServerChooserKey(msg tea.KeyMsg) (tea.Model, tea.Cmd) {
	switch {
	case key.Matches(msg, keys.quit):
		m.close()
		return m, tea.Quit
	case key.Matches(msg, keys.discard):
		m.serverChooser = false
		m.notice = "Server selection unchanged."
		return m, nil
	case key.Matches(msg, keys.rows):
		step := 1
		if reverse(msg) {
			step = -1
		}
		m.serverRow = clamp(m.serverRow+step, 0, len(m.preparedRun.Catalog.Servers)-1)
	case key.Matches(msg, keys.toggleServer):
		id := m.preparedRun.Catalog.Servers[m.serverRow].ID
		if slices.Contains(m.serverDraft, id) {
			m.serverDraft = slices.DeleteFunc(slices.Clone(m.serverDraft), func(value string) bool { return value == id })
		} else if len(m.serverDraft) < 4 {
			m.serverDraft = append(slices.Clone(m.serverDraft), id)
		} else {
			m.notice = "At most four servers may share one test."
		}
	case key.Matches(msg, keys.apply):
		if err := m.controller.SelectServers(m.serverDraft); err != nil {
			m.notice = err.Error()
			return m, nil
		}
		m.cfg.ServerIDs = slices.Clone(m.serverDraft)
		m.serverChooser = false
		m.notice = "Server selection applied. Checking the selected paths…"
		return m.reprepare(nil)
	}
	return m, nil
}
func (m model) useAutomatic() (tea.Model, tea.Cmd) {
	m.cfg.ThroughputTarget = "auto"
	m.cfg.ThroughputProtocol = "auto"
	m.cfg.ThroughputTransport = "auto"
	m.cfg.LatencyTarget = "auto"
	m.cfg.LatencyTransport = "auto"
	m.notice = "Automatic paths applied to every selected server."
	return m.reprepare(nil)
}
func (m model) sharedPaths(latency bool) []pathChoice {
	kinds := []string{wire.TransportFetchStream, wire.TransportWebTransport}
	if latency {
		kinds = []string{wire.TransportWebSocket, wire.TransportWebTransport}
	}
	choices := []pathChoice{automaticPath("each selected server")}
	for _, kind := range kinds {
		var unavailable []string
		if m.preparedRun != nil {
			for _, server := range m.preparedRun.Servers {
				var pf wire.Preflight
				if server.Connection != nil {
					pf = server.Connection.Preflight
				} else if failed, ok := errors.AsType[*goclient.PreparationError](server.Err); ok {
					pf = failed.Preflight
				}
				supported := false
				if latency {
					supported = slices.ContainsFunc(pf.Capabilities.LatencyTargets, func(t wire.LatencyTarget) bool { return t.Transport == kind })
				} else {
					supported = slices.ContainsFunc(pf.Capabilities.ThroughputTargets, func(t wire.ThroughputTarget) bool { return t.Transport == kind })
				}
				if !supported {
					unavailable = append(unavailable, server.Server.Name)
				}
			}
		}
		note := "all selected servers"
		if len(unavailable) > 0 {
			note = "unavailable on " + strings.Join(unavailable, ", ")
		}
		choices = append(choices, pathChoice{target: "auto", transport: kind, label: strings.ReplaceAll(kind, "-", " "), note: note})
	}
	return choices
}
func (m model) serverReadiness(id string) string {
	if m.preparedRun != nil {
		for _, server := range m.preparedRun.Servers {
			if server.Server.ID != id {
				continue
			}
			if server.Err == nil && server.Connection != nil {
				return "Ready"
			}
			if _, ok := errors.AsType[*goclient.AuthRequiredError](server.Err); ok {
				return "Sign in"
			}
			if server.Err != nil {
				return "Unavailable"
			}
		}
	}
	return "Not checked"
}
func (m model) serverChooserView(w int) string {
	if m.preparedRun == nil {
		return "Loading server catalogue…"
	}
	catalog := m.preparedRun.Catalog
	lines := []string{fitLine(accentStyle.Render(fmt.Sprintf("Servers · %d selected", len(m.serverDraft))), w), fitLine(mutedStyle.Render("Combined throughput · separate latency populations"), w), ""}
	capacity := max(3, min(12, (max(m.height, 20)-10)/2))
	start := clamp(m.serverRow-capacity/2, 0, max(0, len(catalog.Servers)-capacity))
	for i := start; i < min(len(catalog.Servers), start+capacity); i++ {
		server := catalog.Servers[i]
		label := server.Name
		if server.ID == "self" {
			label += " · This server"
		}
		if server.Location != "" {
			label += " · " + server.Location
		}
		line := checkbox(slices.Contains(m.serverDraft, server.ID)) + " " + label + " · " + m.serverReadiness(server.ID)
		if i == m.serverRow {
			line = "› " + selectedStyle.Render(line)
		} else {
			line = "  " + line
		}
		lines = append(lines, fitLine(line, w), fitLine("    "+mutedStyle.Render(server.URL), w))
	}
	lines = append(lines, "", fitLine(m.notice, w))
	return strings.Join(lines, "\n")
}
func (m *model) nextLatencyFocus() {
	if m.runDetails == nil || len(m.runDetails.Selection) < 2 {
		return
	}
	ids := make([]string, len(m.runDetails.Selection))
	for i, server := range m.runDetails.Selection {
		ids[i] = server.ID
	}
	m.latencyFocus = ids[(slices.Index(ids, m.latencyFocus)+1)%len(ids)]
	m.latency = m.latencyByServer[m.latencyFocus]
	m.lostStreak = m.lostByServer[m.latencyFocus]
}
func (m model) latencyServerName() string {
	if m.runDetails != nil {
		for _, server := range m.runDetails.Selection {
			if server.ID == m.latencyFocus {
				return server.Name
			}
		}
	}
	return ""
}
func (m model) visibleResults() []goclient.Result {
	results := slices.Clone(m.results)
	results = append(results, m.serverResults[m.latencyFocus]...)
	return results
}
func (m model) serverResultsView(w int) string {
	details := m.runDetails
	if details == nil {
		return ""
	}
	notice := fmt.Sprintf("%d selected servers", len(details.Selection))
	if details.Outcome == "incomplete" {
		notice = "Measurement incomplete"
	} else if details.Outcome != "running" && len(details.Participants) < len(details.Selection) {
		notice = fmt.Sprintf("Completed with %d of %d servers", len(details.Participants), len(details.Selection))
	} else if len(details.Failures) > 0 {
		notice = "Latency interrupted"
	}
	lines := []string{accentStyle.Render(notice), mutedStyle.Render("Per-server contributions while sharing the connection")}
	for _, server := range details.Servers {
		var contributions []string
		for _, result := range server.Results {
			if result.Direction == "" {
				continue
			}
			rate := "--"
			if !result.Unavailable {
				rate = fmtRate(result.MeanBps)
			}
			contributions = append(contributions, result.Stage+" "+string(result.Direction)+" "+rate)
		}
		lines = append(lines, fitLine(server.Server.Name+" · "+strings.Join(contributions, " · "), w))
	}
	for _, failure := range details.Failures {
		name := failure.ServerID
		for _, server := range details.Selection {
			if server.ID == failure.ServerID {
				name = server.Name
			}
		}
		lines = append(lines, fitLine(fmt.Sprintf("%s · %s %s · %.1fs · %s", name, failure.Stage, failure.Scope, failure.At.Seconds(), failure.Message), w))
	}
	if details.Outcome != "running" {
		for _, interval := range details.Intervals {
			state := "incomplete evidence"
			if interval.Complete && interval.Window != nil {
				state = "measured window"
			}
			lines = append(lines, fitLine(fmt.Sprintf("%s %.1f–%.1fs · %s · %s", interval.Stage, interval.Start.Seconds(), interval.End.Seconds(), strings.Join(interval.Participants, ", "), state), w))
		}
	}
	if details.OmittedIntervals > 0 {
		lines = append(lines, fmt.Sprintf("%d older intervals omitted; byte totals retain the full run", details.OmittedIntervals))
	}
	return strings.Join(lines, "\n")
}

func (m model) serverResultNotice() string {
	details := m.runDetails
	if details == nil {
		return ""
	}
	if details.Outcome == "incomplete" {
		return "Measurement incomplete"
	}
	if len(details.Participants) < len(details.Selection) {
		if m.complete {
			return fmt.Sprintf("Completed with %d of %d servers", len(details.Participants), len(details.Selection))
		}
		return fmt.Sprintf("%d of %d servers remaining", len(details.Participants), len(details.Selection))
	}
	if len(details.Failures) > 0 {
		return "Latency interrupted"
	}
	return fmt.Sprintf("%d selected servers", len(details.Selection))
}
func (m model) handleServerDetailsKey(msg tea.KeyMsg) (tea.Model, tea.Cmd) {
	switch {
	case key.Matches(msg, keys.quit):
		m.close()
		return m, tea.Quit
	case key.Matches(msg, keys.discard) || key.Matches(msg, keys.serverDetails):
		m.serverDetailsOpen = false
	case key.Matches(msg, keys.rows):
		step := 1
		if reverse(msg) {
			step = -1
		}
		m.detailsScroll = clamp(m.detailsScroll+step, 0, max(0, len(strings.Split(m.serverResultsView(m.innerWidth()), "\n"))-max(3, m.height-9)))
	}
	return m, nil
}
func (m model) serverDetailsOverlay(w int) string {
	lines := strings.Split(m.serverResultsView(w), "\n")
	capacity := max(3, m.height-9)
	start := clamp(m.detailsScroll, 0, max(0, len(lines)-capacity))
	return strings.Join(lines[start:min(len(lines), start+capacity)], "\n") + "\n\n" + mutedStyle.Render("↑/↓ scroll · esc closes details")
}
