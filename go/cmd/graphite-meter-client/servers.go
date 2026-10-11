package main

import (
	"fmt"
	"slices"
	"strings"

	tea "charm.land/bubbletea/v2"
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
	state  pathState
	detail string
}

func (m model) readiness() []readiness {
	if m.preparedRun == nil {
		return nil
	}
	out := make([]readiness, 0, len(m.preparedRun.Servers))
	for _, s := range m.preparedRun.Servers {
		r := readiness{server: s.Server}
		switch {
		case m.prepare == prepareChecking:
			r.state = pathChecking
		case goclient.IsAuthRequired(s.Err):
			r.state = pathSignIn
		case s.Err != nil || s.Connection == nil:
			r.state = pathFailed
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
		if r.state == pathReady {
			ids = append(ids, r.server.ID)
		}
	}
	return ids
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
		return ""
	}
	return fmt.Sprintf("%d of %d ready", ready, len(rows))
}

// openServerChooser opens the chooser over the catalogue, or once the check in progress brings it.
func (m model) openServerChooser() (tea.Model, tea.Cmd) {
	switch {
	case m.prepare == prepareChecking:
		m.openChooser = true
		return m.notify(styled("Test servers open once the check finishes.", m.pal.muted)), nil
	case m.preparedRun == nil:
		m.openChooser = true
		m = m.notify(styled("Test servers open once the check finishes.", m.pal.muted))
		return m.reprepare()
	case !m.canChooseServers():
		return m.notify(styled("This server offers one test server.", m.pal.muted)), nil
	}
	m.overlay, m.serverRow, m.serverDraft, m.scroll = overlayChooser, 0, nil, 0
	ids := m.cfg.ServerIDs
	if len(ids) == 0 {
		ids = m.preparedRun.Catalog.DefaultSelection
	}
	for _, server := range m.preparedRun.Catalog.Servers {
		if slices.Contains(ids, server.ID) {
			m.serverDraft = append(m.serverDraft, server.ID)
		}
	}
	return m, nil
}

func (m model) handleServerChooserKey(a action, msg tea.KeyPressMsg) (tea.Model, tea.Cmd) {
	servers := m.preparedRun.Catalog.Servers
	switch a {
	case actCancel:
		m.overlay = overlaySettings
	case actMove:
		m.serverRow = min(max(m.serverRow+delta(msg), 0), len(servers)-1)
	case actSelect:
		m.selectEntry()
	case actApply:
		m.cfg.ServerIDs = slices.Clone(m.serverDraft)
		m.overlay = overlaySettings
		return m.reprepare()
	}
	return m, nil
}

// selectEntry selects the focused entry, or deselects it; at most four share a test.
func (m *model) selectEntry() {
	id := m.preparedRun.Catalog.Servers[m.serverRow].ID
	switch {
	case slices.Contains(m.serverDraft, id):
		m.serverDraft = slices.DeleteFunc(slices.Clone(m.serverDraft), func(v string) bool { return v == id })
	case len(m.serverDraft) < wire.MaxSelectedServers:
		m.serverDraft = append(slices.Clone(m.serverDraft), id)
	default:
		*m = m.notify(styled("At most four servers share one test.", m.pal.warn))
	}
}

// chooserEntries are the catalogue as the chooser's entries, each with its readiness, and the focused one's index.
func (m model) chooserEntries() ([]entry, int) {
	p := m.pal
	entries := []entry{{text: styled("Choose up to four. Their speeds add up.", p.muted)}}
	focus := 0
	states := m.readiness()
	for i, server := range m.preparedRun.Catalog.Servers {
		if i == m.serverRow {
			focus = len(entries)
		}
		mark := styled("○", p.muted)
		if slices.Contains(m.serverDraft, server.ID) {
			mark = styled("●", p.accent)
		}
		value := line{}
		if r := slices.IndexFunc(states, func(r readiness) bool { return r.server.ID == server.ID }); r >= 0 {
			value = styled(pathLabels[states[r].state], p.muted)
		}
		entries = append(entries, entry{item: true, label: mark.and(" ", style{}).and(serverLabel(server.Name,
			server.Location), p.text), value: value, target: target{kind: targetEntry, index: i}})
	}
	return entries, focus
}
