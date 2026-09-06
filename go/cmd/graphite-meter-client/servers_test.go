package main

import (
	"fmt"
	"slices"
	"strings"
	"testing"

	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func chooserModel(t *testing.T) model {
	t.Helper()
	m := newModel(goclient.DefaultConfig())
	t.Cleanup(m.close)
	catalog := wire.SingletonCatalog()
	for i := range 8 {
		catalog.Servers = append(catalog.Servers, wire.ServerEntry{ID: fmt.Sprint(i), URL: fmt.Sprintf("https://server-%d.example", i), Name: fmt.Sprintf("Server %d", i)})
	}
	m.preparedRun = &goclient.PreparedRun{Catalog: catalog}
	return m
}
func TestServerChooserDraftKeyboardAndLimit(t *testing.T) {
	m := chooserModel(t)
	next, _ := m.openServerChooser()
	m = next.(model)
	if !m.serverChooser || !slices.Equal(m.serverDraft, []string{"self"}) {
		t.Fatal("operator default was not copied into draft")
	}
	key := func(k tea.KeyType) { next, _ = m.handleServerChooserKey(tea.KeyMsg{Type: k}); m = next.(model) }
	key(tea.KeySpace)
	if len(m.serverDraft) != 0 {
		t.Fatal("self could not be deselected")
	}
	for range 5 {
		key(tea.KeyDown)
		key(tea.KeySpace)
	}
	if len(m.serverDraft) != 4 || len(m.cfg.ServerIDs) != 0 {
		t.Fatal("draft escaped limit or changed the applied selection")
	}
	key(tea.KeyEsc)
	if m.serverChooser || len(m.cfg.ServerIDs) != 0 {
		t.Fatal("Escape committed draft")
	}
	m.mode = modeRun
	m.complete = false
	next, _ = m.openServerChooser()
	if next.(model).serverChooser {
		t.Fatal("selection changed while measuring")
	}
}
func TestServerChooserAndResultDetailsFitNarrowTerminal(t *testing.T) {
	m := chooserModel(t)
	m.height = 20
	m.width = 44
	m.serverDraft = []string{"self", "0"}
	m.serverRow = 7
	for _, line := range strings.Split(m.serverChooserView(38), "\n") {
		if lipgloss.Width(line) > 38 {
			t.Fatalf("chooser overflow: %q", line)
		}
	}
	m.complete = true
	m.runDetails = &goclient.RunDetails{Selection: m.preparedRun.Catalog.Servers[:3], Participants: []string{"0", "1"}, Outcome: "partial"}
	if m.serverResultNotice() != "Completed with 2 of 3 servers" {
		t.Fatal(m.serverResultNotice())
	}
	m.detailsScroll = 100
	if len(strings.Split(m.serverDetailsOverlay(38), "\n")) > m.height-6 {
		t.Fatal("details exceeded viewport")
	}
}
func TestLatencyFocusSwitchesTheWholePopulation(t *testing.T) {
	m := chooserModel(t)
	m.runDetails = &goclient.RunDetails{Selection: []wire.ServerEntry{{ID: "a", Name: "A"}, {ID: "b", Name: "B"}}}
	m.latencyFocus = "a"
	m.latencyByServer = map[string]goclient.LatencySample{"a": {RTT: 10}, "b": {RTT: 90}}
	m.lostByServer = map[string]int{"a": 0, "b": 3}
	m.nextLatencyFocus()
	if m.latencyFocus != "b" || m.latency.RTT != 90 || m.lostStreak != 3 || m.latencyServerName() != "B" {
		t.Fatalf("mixed latency population: %+v", m.latency)
	}
}
