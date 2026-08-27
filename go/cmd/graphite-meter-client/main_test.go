package main

import (
	"errors"
	"regexp"
	"strings"
	"testing"
	"time"

	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
	"github.com/muesli/termenv"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

// --- pure helpers ---

func TestParseStages(t *testing.T) {
	cases := []struct {
		name string
		raw  string
		want goclient.StageSet
	}{
		{"all long names", "latency,download,upload,bidirectional", goclient.StageSet{Latency: true, Download: true, Upload: true, Bidirectional: true}},
		{"aliases", "ping,down,up,bidi", goclient.StageSet{Latency: true, Download: true, Upload: true, Bidirectional: true}},
		{"empty", "", goclient.StageSet{}},
		{"unknown token ignored", "bogus", goclient.StageSet{}},
		{"mixed known and unknown", "download,bogus", goclient.StageSet{Download: true}},
		{"trims and lowercases", " Latency , DOWN ", goclient.StageSet{Latency: true, Download: true}},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			got := parseStages(c.raw)
			if got != c.want {
				t.Errorf("parseStages(%q) = %+v, want %+v", c.raw, got, c.want)
			}
		})
	}
}

func TestParsePing(t *testing.T) {
	cases := []struct {
		name string
		raw  string
		want time.Duration
	}{
		{"instant", "instant", 80 * time.Millisecond},
		{"slow", "slow", 600 * time.Millisecond},
		{"case insensitive slow", "Slow", 600 * time.Millisecond},
		{"medium", "medium", 250 * time.Millisecond},
		{"empty defaults to medium", "", 250 * time.Millisecond},
		{"valid duration", "1500ms", 1500 * time.Millisecond},
		{"zero falls back", "0s", 250 * time.Millisecond},
		{"negative falls back", "-10s", 250 * time.Millisecond},
		{"unparseable falls back", "bogus", 250 * time.Millisecond},
		{"the bound itself passes", goclient.MaxPingInterval.String(), goclient.MaxPingInterval},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			got, err := parsePing(c.raw, "auto")
			if err != nil {
				t.Fatalf("parsePing(%q): %v", c.raw, err)
			}
			if got != c.want {
				t.Errorf("parsePing(%q) = %v, want %v", c.raw, got, c.want)
			}
		})
	}
}

func TestParsePingBindsTheCadenceToTheSelectedBus(t *testing.T) {
	got, err := parsePing("45s", wire.TransportWebTransport)
	if err == nil {
		t.Fatalf("parsePing(\"45s\", WebTransport) = %v, want an error naming the bound", got)
	}
	if !strings.Contains(err.Error(), goclient.MaxPingInterval.String()) {
		t.Errorf("WebTransport error = %q, want it to name the %v bound", err, goclient.MaxPingInterval)
	}

	for _, transport := range []string{"auto", wire.TransportWebSocket} {
		got, err := parsePing("45s", transport)
		if err != nil || got != 45*time.Second {
			t.Errorf("parsePing(\"45s\", %q) = %v, %v, want deferred/accepted 45s", transport, got, err)
		}
	}
}

func TestFlagsRejectAnUnknownTransport(t *testing.T) {
	cases := []struct {
		name                string
		throughput, latency string
		wantFlag            string
	}{
		{"both default", "auto", "auto", ""},
		{"both named", wire.TransportWebTransport, wire.TransportWebSocket, ""},
		{"fetch stream", wire.TransportFetchStream, "auto", ""},
		{"throughput typo", "webscoket", "auto", "-throughput-transport"},
		{"latency typo", "auto", "websockets", "-latency-transport"},
		{"latency datagram is not a bus", "auto", wire.TransportWebTransportDatagram, "-latency-transport"},
		{"throughput datagram passes the flag", wire.TransportWebTransportDatagram, "auto", ""},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			err := transportFlags(c.throughput, c.latency)
			if c.wantFlag == "" {
				if err != nil {
					t.Fatalf("transportFlags(%q, %q) = %v, want it accepted", c.throughput, c.latency, err)
				}
				return
			}
			if err == nil {
				t.Fatalf("transportFlags(%q, %q) accepted an unknown transport", c.throughput, c.latency)
			}
			if !strings.Contains(err.Error(), c.wantFlag) {
				t.Errorf("error = %q, want it to name %s", err, c.wantFlag)
			}
		})
	}
}

func TestClamp(t *testing.T) {
	cases := []struct {
		name        string
		v, min, max int
		want        int
	}{
		{"in range", 5, 0, 10, 5},
		{"below min", -1, 0, 10, 0},
		{"above max", 11, 0, 10, 10},
		{"min equals max, in range", 3, 3, 3, 3},
		{"min equals max, below", 2, 3, 3, 3},
		{"min equals max, above", 5, 3, 3, 3},
		{"max less than min returns min", 5, 5, 2, 5},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			if got := clamp(c.v, c.min, c.max); got != c.want {
				t.Errorf("clamp(%d, %d, %d) = %d, want %d", c.v, c.min, c.max, got, c.want)
			}
		})
	}
}

func TestFormatting(t *testing.T) {
	cases := []struct {
		name string
		got  func() string
		want string
	}{
		{"rate zero", func() string { return fmtRate(0) }, "0 bit/s"},
		{"rate bits", func() string { return fmtRate(12.5) }, "100 bit/s"},
		{"rate kilo", func() string { return fmtRate(125) }, "1.00 Kbit/s"},
		{"rate mega", func() string { return fmtRate(125000) }, "1.00 Mbit/s"},
		{"rate giga", func() string { return fmtRate(125000000) }, "1.00 Gbit/s"},
		{"rate tera", func() string { return fmtRate(125000000000) }, "1.00 Tbit/s"},
		{"rate caps at tera", func() string { return fmtRate(125000000000000) }, "1000.00 Tbit/s"},
		{"bytes zero", func() string { return fmtBytes(0) }, "0 B"},
		{"bytes raw", func() string { return fmtBytes(500) }, "500 B"},
		{"bytes kilo", func() string { return fmtBytes(1000) }, "1.00 KB"},
		{"bytes mega", func() string { return fmtBytes(1500000) }, "1.50 MB"},
		{"bytes giga", func() string { return fmtBytes(1250000000) }, "1.25 GB"},
		{"bytes caps at tera", func() string { return fmtBytes(1000000000000000) }, "1000.00 TB"},
		{"milliseconds zero", func() string { return fmtMs(0) }, "--"},
		{"milliseconds negative", func() string { return fmtMs(-time.Millisecond) }, "--"},
		{"milliseconds fractional", func() string { return fmtMs(1500 * time.Microsecond) }, "1.50 ms"},
		{"milliseconds seconds", func() string { return fmtMs(2 * time.Second) }, "2000.00 ms"},
		{"milliseconds mixed", func() string { return fmtMs(3*time.Millisecond + 250*time.Microsecond) }, "3.25 ms"},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			if got := c.got(); got != c.want {
				t.Errorf("format = %q, want %q", got, c.want)
			}
		})
	}
}

func TestStageSummary(t *testing.T) {
	cases := []struct {
		name string
		s    goclient.StageSet
		want string
	}{
		{"none", goclient.StageSet{}, "none"},
		{"single", goclient.StageSet{Latency: true}, "latency"},
		{"pair", goclient.StageSet{Download: true, Upload: true}, "download, upload"},
		{"all", goclient.StageSet{Latency: true, Download: true, Upload: true, Bidirectional: true}, "latency, download, upload, bidirectional"},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			if got := stageSummary(c.s); got != c.want {
				t.Errorf("stageSummary(%+v) = %q, want %q", c.s, got, c.want)
			}
		})
	}
}

func TestRunOrder(t *testing.T) {
	cfg := goclient.DefaultConfig()
	cfg.Stages = goclient.StageSet{}
	lines := runOrder(cfg)
	if len(lines) != 1 || !strings.Contains(lines[0], "No stages selected") {
		t.Errorf("runOrder with no stages = %v, want single 'No stages selected' line", lines)
	}

	cfg.Stages = goclient.StageSet{Latency: true, Download: true}
	cfg.LatencyDuration = 2 * time.Second
	cfg.DownloadDuration = 5 * time.Second
	lines = runOrder(cfg)
	want := []string{"latency        2s", "download       5s"}
	if len(lines) != len(want) {
		t.Fatalf("runOrder = %v, want %v", lines, want)
	}
	for i := range want {
		if lines[i] != want[i] {
			t.Errorf("runOrder[%d] = %q, want %q", i, lines[i], want[i])
		}
	}
}

func TestActivePreset(t *testing.T) {
	for i, preset := range serverPresets {
		if got := activePreset(preset.url); got != i {
			t.Errorf("activePreset(%q) = %d, want %d", preset.url, got, i)
		}
	}
	if got := activePreset("http://example.invalid:9999"); got != -1 {
		t.Errorf("activePreset(unknown) = %d, want -1", got)
	}
}

func TestTimingLabel(t *testing.T) {
	want := []string{"Warmup", "Latency", "Download", "Upload", "Bidirectional", "Ping interval"}
	for i, w := range want {
		if got := timingLabel(i); got != w {
			t.Errorf("timingLabel(%d) = %q, want %q", i, got, w)
		}
	}
	if got := timingLabel(99); got != "" {
		t.Errorf("timingLabel(out of range) = %q, want empty", got)
	}
}

func TestProtocolChoiceLabelNamesEveryVersionTheRowOffers(t *testing.T) {
	for raw, want := range map[string]string{
		"auto": "Automatic", "http1": "HTTP/1.1", "http2": "HTTP/2",
		"http3": "HTTP/3", protocolNegotiated: "Negotiated",
		"": "--", "http9": "http9",
	} {
		if got := protocolChoiceLabel(raw); got != want {
			t.Errorf("protocolChoiceLabel(%q) = %q, want %q", raw, got, want)
		}
	}
}

func TestUnofferedPathLabelSpellsOutWhatWasAskedFor(t *testing.T) {
	for _, c := range []struct{ target, transport, want string }{
		{"auto", "auto", "Automatic"},
		{"auto", wire.TransportWebTransport, "WebTransport · automatic origin"},
		{"https://meter.example", "auto", "Automatic transport · https://meter.example"},
		{"https://meter.example", wire.TransportFetchStream, "Fetch stream · https://meter.example"},
		{"https://meter.example", "quic-v9", "quic-v9 · https://meter.example"},
	} {
		if got := unofferedPathLabel(c.target, c.transport); got != c.want {
			t.Errorf("unofferedPathLabel(%q, %q) = %q, want %q", c.target, c.transport, got, c.want)
		}
	}
}

func TestLaunchChecksNothingUntilAServerIsPicked(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	if cmd := m.Init(); cmd != nil {
		t.Fatal("launch opened a connection on its own")
	}
	if m.prepareStatus != statusIdle {
		t.Fatalf("launch status = %q, want %q", m.prepareStatus, statusIdle)
	}
	for _, c := range m.connectionChecks() {
		if c.state != checkPending {
			t.Errorf("check %q = %v at launch, want pending", c.label, c.state)
		}
	}

	// The preset already holds the configured URL, so nothing changes but the check still runs.
	m.section, m.row = sectionServers, 0
	m.cfg.BaseURL = serverPresets[0].url
	m, cmd := modelAndCmd(m.confirm())
	if cmd == nil || m.prepareStatus != "checking" {
		t.Fatalf("picking the current server left status %q with cmd %v", m.prepareStatus, cmd != nil)
	}

	// So does applying the URL editor over an unchanged URL.
	m.prepareStatus = statusIdle
	m.edit = beginEdit(editURL, "url", m.cfg.BaseURL)
	m, cmd = modelAndCmd(m.handleEditKey(tea.KeyMsg{Type: tea.KeyEnter}))
	if cmd == nil || m.prepareStatus != "checking" {
		t.Fatalf("reapplying the same URL left status %q with cmd %v", m.prepareStatus, cmd != nil)
	}
}

func TestPreparationMessageIgnoresOldGenerationAndPublishesFailure(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.prepareSeq = 2
	m.prepareStatus = "checking"

	m, _ = modelAndCmd(m.Update(preparationMsg{seq: 1, err: errors.New("old")}))
	if m.prepareStatus != "checking" {
		t.Fatalf("stale preparation changed status to %q", m.prepareStatus)
	}

	m, _ = modelAndCmd(m.Update(preparationMsg{seq: 2, err: errors.New("unreachable")}))
	if m.prepareStatus != "failed" || !strings.Contains(m.prepareError, "unreachable") {
		t.Fatalf("failure state = %q %q", m.prepareStatus, m.prepareError)
	}
}

func TestRecheckInvalidatesTheInFlightPreparation(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	superseded := m.prepareSeq

	m, _ = modelAndCmd(m.handleKey(keyRunes("v")))
	if m.prepareSeq == superseded {
		t.Fatalf("recheck kept the preparation sequence at %d", m.prepareSeq)
	}

	m, _ = modelAndCmd(m.Update(preparationMsg{seq: superseded, connection: &goclient.PreparedConnection{}}))
	if m.prepareStatus != "checking" || m.prepared != nil {
		t.Fatalf("superseded preparation was adopted: status=%q prepared=%v", m.prepareStatus, m.prepared)
	}

	m, _ = modelAndCmd(m.Update(preparationMsg{seq: m.prepareSeq, connection: &goclient.PreparedConnection{}}))
	if m.prepareStatus != "ready" || m.prepared == nil {
		t.Fatalf("current preparation was not adopted: status=%q prepared=%v", m.prepareStatus, m.prepared)
	}
}

func TestEndpointCyclingChecksOnceAndHoldsTheLastFigures(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.section, m.row = sectionConnections, rowThroughputProtocol
	m, _ = modelAndCmd(m.Update(preparationMsg{seq: m.prepareSeq, connection: &goclient.PreparedConnection{}}))

	var due []prepareDueMsg
	for range 3 {
		m, _ = modelAndCmd(m.Update(tea.KeyMsg{Type: tea.KeyEnter}))
		due = append(due, prepareDueMsg{seq: m.prepareSeq})
	}
	if m.cfg.ThroughputProtocol != "http3" {
		t.Fatalf("three presses left the protocol at %q, want the third choice", m.cfg.ThroughputProtocol)
	}
	if m.prepared == nil {
		t.Error("the last verified connection was dropped, so the panel has nothing to show")
	}

	for _, msg := range due[:len(due)-1] {
		if _, cmd := m.Update(msg); cmd != nil {
			t.Errorf("superseded debounce %d opened a connection", msg.seq)
		}
	}
	if _, cmd := m.Update(due[len(due)-1]); cmd == nil {
		t.Error("the last change never reached the network")
	}
}

func TestPreparationFailureKeepsDiscoveredTargetsSelectable(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.section = sectionConnections
	m.row = rowThroughputPath
	pf := wire.Preflight{Capabilities: wire.Capabilities{
		ThroughputTargets: []wire.ThroughputTarget{
			{ID: "https://one.example", Origin: "https://one.example", Transport: wire.TransportFetchStream, Protocol: "http2"},
			{ID: "https://two.example", Origin: "https://two.example", Transport: wire.TransportFetchStream, Protocol: "http2"},
		},
	}}

	m, _ = modelAndCmd(m.Update(preparationMsg{
		seq: m.prepareSeq,
		err: &goclient.PreparationError{
			Preflight: pf,
			Err:       errors.New("multiple throughput endpoints available; select an origin"),
		},
	}))
	m, _ = modelAndCmd(m.activate())
	if got, want := m.cfg.ThroughputTarget, "https://one.example"; got != want {
		t.Fatalf("selected throughput target = %q, want %q", got, want)
	}
	if got, want := m.cfg.ThroughputTransport, wire.TransportFetchStream; got != want {
		t.Fatalf("selected throughput transport = %q, want %q", got, want)
	}
}

func TestPathPickerOffersEachMechanismAndDeduplicatesOrigins(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.section = sectionConnections
	m.row = rowThroughputPath
	m.discovery = &wire.Preflight{Capabilities: wire.Capabilities{
		ThroughputTargets: []wire.ThroughputTarget{
			{Origin: "https://meter.example:443", Transport: wire.TransportFetchStream, Protocol: "http2", TLS: true},
			{Origin: "https://meter.example", Transport: wire.TransportFetchStream, Protocol: "http2", TLS: true},
			{Origin: "https://meter.example:7249", Transport: wire.TransportWebTransport, Protocol: "http3", TLS: true},
			// The client refuses the datagram flood, so it is never offered.
			{Origin: "https://meter.example:7249", Transport: wire.TransportWebTransportDatagram, Protocol: "http3", TLS: true},
		},
		LatencyTargets: []wire.LatencyTarget{
			{Origin: "http://meter.example:80", Transport: wire.TransportWebSocket, Protocol: "http1"},
			{Origin: "http://meter.example", Transport: wire.TransportWebSocket, Protocol: "http1"},
			{Origin: "https://meter.example:7249", Transport: wire.TransportWebTransport, Protocol: "http3", TLS: true},
		},
	}}

	want := []struct{ target, transport string }{
		{"https://meter.example:443", wire.TransportFetchStream},
		{"https://meter.example:7249", wire.TransportWebTransport},
		{"auto", "auto"},
	}
	for i, w := range want {
		m, _ = modelAndCmd(m.activate())
		if m.cfg.ThroughputTarget != w.target || m.cfg.ThroughputTransport != w.transport {
			t.Fatalf("throughput press %d = %q over %q, want %q over %q", i+1, m.cfg.ThroughputTarget, m.cfg.ThroughputTransport, w.target, w.transport)
		}
	}

	m.row = rowLatencyPath
	wantLatency := []struct{ target, transport string }{
		{"http://meter.example:80", wire.TransportWebSocket},
		{"https://meter.example:7249", wire.TransportWebTransport},
		{"auto", "auto"},
	}
	for i, w := range wantLatency {
		m, _ = modelAndCmd(m.activate())
		if m.cfg.LatencyTarget != w.target || m.cfg.LatencyTransport != w.transport {
			t.Fatalf("latency press %d = %q over %q, want %q over %q", i+1, m.cfg.LatencyTarget, m.cfg.LatencyTransport, w.target, w.transport)
		}
	}
}

func TestCheckbox(t *testing.T) {
	if got := checkbox(true); got != "●" {
		t.Errorf("checkbox(true) = %q, want ●", got)
	}
	if got := checkbox(false); got != "○" {
		t.Errorf("checkbox(false) = %q, want ○", got)
	}
}

func TestDurationValue(t *testing.T) {
	cfg := goclient.DefaultConfig()
	cfg.Warmup = time.Second
	cfg.LatencyDuration = 2 * time.Second
	cfg.DownloadDuration = 3 * time.Second
	cfg.UploadDuration = 4 * time.Second
	cfg.BidirectionalDuration = 5 * time.Second
	cfg.PingInterval = 6 * time.Second
	m := newModel(cfg)

	want := []string{"1s", "2s", "3s", "4s", "5s", "6s"}
	for i, w := range want {
		if got := m.durationValue(i); got != w {
			t.Errorf("durationValue(%d) = %q, want %q", i, got, w)
		}
	}
	if got := m.durationValue(99); got != "" {
		t.Errorf("durationValue(out of range) = %q, want empty", got)
	}
}

func TestRowCount(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	cases := []struct {
		section section
		want    int
	}{
		{sectionServers, len(serverPresets) + 1},
		{sectionRunSetup, 5},
		{sectionTiming, 6},
		{sectionConnections, rowConnectionsCount},
		{sectionRun, 1},
	}
	for _, c := range cases {
		m.section = c.section
		if got := m.rowCount(); got != c.want {
			t.Errorf("rowCount() with section %v = %d, want %d", c.section, got, c.want)
		}
	}
}

// --- model state machine ---

func TestNewModel(t *testing.T) {
	cfg := goclient.DefaultConfig()
	m := newModel(cfg)
	if m.mode != modeConfigure {
		t.Errorf("newModel mode = %v, want modeConfigure", m.mode)
	}
	if m.notice == "" {
		t.Error("newModel notice should not be empty")
	}
	if m.rates == nil || m.peaks == nil {
		t.Error("newModel should initialize rates and peaks maps")
	}
}

func keyRunes(s string) tea.KeyMsg {
	return tea.KeyMsg{Type: tea.KeyRunes, Runes: []rune(s)}
}

// keyPaste is what a bracketed paste delivers: every rune in one message.
func keyPaste(s string) tea.KeyMsg {
	return tea.KeyMsg{Type: tea.KeyRunes, Runes: []rune(s), Paste: true}
}

func modelAndCmd(next tea.Model, cmd tea.Cmd) (model, tea.Cmd) { return next.(model), cmd }

func TestHandleKey_SectionNavigation(t *testing.T) {
	cases := []struct {
		name string
		keys []tea.KeyMsg
		want []section
	}{
		{"tab cycles", []tea.KeyMsg{{Type: tea.KeyTab}}, []section{sectionRunSetup, sectionTiming, sectionConnections, sectionRun, sectionServers}},
		{"shift-tab", []tea.KeyMsg{{Type: tea.KeyShiftTab}}, []section{sectionRun}},
		{"right", []tea.KeyMsg{{Type: tea.KeyRight}}, []section{sectionRunSetup}},
		{"right then left", []tea.KeyMsg{{Type: tea.KeyRight}, {Type: tea.KeyLeft}}, []section{sectionRunSetup, sectionServers}},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			m := newModel(goclient.DefaultConfig())
			for i, want := range c.want {
				m, _ = modelAndCmd(m.handleKey(c.keys[i%len(c.keys)]))
				if m.section != want {
					t.Errorf("section after %s = %v, want %v", c.name, m.section, want)
				}
			}
		})
	}
}

func TestHandleKey_RowNavigationClamped(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.section = sectionRunSetup // rowCount == 5, valid rows 0..4
	m.row = 4

	steps := []struct {
		name string
		key  tea.KeyMsg
		want int
	}{
		{"down at max", tea.KeyMsg{Type: tea.KeyDown}, 4},
		{"up", tea.KeyMsg{Type: tea.KeyUp}, 3},
		{"k", keyRunes("k"), 2},
		{"j", keyRunes("j"), 3},
	}
	for _, step := range steps {
		m, _ = modelAndCmd(m.handleKey(step.key))
		if m.row != step.want {
			t.Errorf("row after %s = %d, want %d", step.name, m.row, step.want)
		}
	}

	m.row = 0
	m, _ = modelAndCmd(m.handleKey(tea.KeyMsg{Type: tea.KeyUp}))
	if m.row != 0 {
		t.Errorf("row after up at min = %d, want clamped to 0", m.row)
	}
}

func TestHandleKey_RowNavigationClampedAcrossSections(t *testing.T) {
	cases := []struct {
		name    string
		section section
		rows    int
	}{
		{"servers", sectionServers, len(serverPresets) + 1},
		{"network", sectionConnections, rowConnectionsCount},
		{"run (single row)", sectionRun, 1},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			m := newModel(goclient.DefaultConfig())
			m.section = c.section

			m.row = 0
			m, _ = modelAndCmd(m.handleKey(tea.KeyMsg{Type: tea.KeyUp}))
			if m.row != 0 {
				t.Errorf("row after up at the first row = %d, want clamped to 0", m.row)
			}

			m.row = c.rows - 1
			m, _ = modelAndCmd(m.handleKey(tea.KeyMsg{Type: tea.KeyDown}))
			if m.row != c.rows-1 {
				t.Errorf("row after down at the last row = %d, want clamped to %d (no wraparound)", m.row, c.rows-1)
			}
		})
	}
}

func TestHandleKey_RapidEditStartCancelReEdit(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.section = sectionServers
	m.row = len(serverPresets) // the "Custom URL" row

	m, _ = modelAndCmd(m.activate())
	if m.edit.kind != editURL {
		t.Fatalf("edit.kind after starting an edit = %v, want editURL", m.edit.kind)
	}
	baseline := m.edit.input.Value()

	m, _ = modelAndCmd(m.handleKey(keyRunes("x")))
	m, _ = modelAndCmd(m.handleKey(tea.KeyMsg{Type: tea.KeyEsc}))
	if m.edit.kind != editNone {
		t.Fatalf("edit.kind after cancel = %v, want editNone", m.edit.kind)
	}
	if m.cfg.BaseURL != baseline {
		t.Errorf("BaseURL changed to %q after a cancelled edit, want unchanged %q", m.cfg.BaseURL, baseline)
	}

	// Re-entering the edit reflects the committed config, not the discarded "x" of a canceled attempt.
	m, _ = modelAndCmd(m.activate())
	if m.edit.input.Value() != baseline {
		t.Errorf("edit value on re-entry = %q, want the unchanged BaseURL %q (not the cancelled edit)", m.edit.input.Value(), baseline)
	}

	m, _ = modelAndCmd(m.handleKey(keyRunes("9")))
	m, _ = modelAndCmd(m.handleKey(tea.KeyMsg{Type: tea.KeyEnter}))
	if m.edit.kind != editNone {
		t.Errorf("edit.kind after commit = %v, want editNone", m.edit.kind)
	}
	if want := baseline + "9"; m.cfg.BaseURL != want {
		t.Errorf("BaseURL after committing the re-edit = %q, want %q", m.cfg.BaseURL, want)
	}
}

func TestHandleKey_QuitSendsCancelAndQuit(t *testing.T) {
	for _, key := range []tea.KeyMsg{{Type: tea.KeyCtrlC}, keyRunes("q")} {
		called := false
		m := newModel(goclient.DefaultConfig())
		m.cancel = func() { called = true }

		_, cmd := m.handleKey(key)
		if !called {
			t.Errorf("key %q did not invoke cancel", key.String())
		}
		if cmd == nil {
			t.Fatalf("key %q returned nil cmd, want tea.Quit", key.String())
		}
		if _, ok := cmd().(tea.QuitMsg); !ok {
			t.Errorf("key %q cmd() did not produce tea.QuitMsg", key.String())
		}
	}
}

func TestHandleKey_ConfigureIgnoresTheRunScreensKeys(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.section = sectionTiming
	m.row = 2
	for _, msg := range []tea.KeyMsg{{Type: tea.KeyEsc}, keyRunes("m"), keyRunes("c")} {
		got, cmd := modelAndCmd(m.handleKey(msg))
		if cmd != nil || got.section != sectionTiming || got.row != 2 || got.edit.kind != editNone {
			t.Errorf("%q changed the configure screen: section=%v row=%d edit=%v", msg.String(), got.section, got.row, got.edit.kind)
		}
	}
}

func quitMsg(cmd tea.Cmd) bool {
	if cmd == nil {
		return false
	}
	_, ok := cmd().(tea.QuitMsg)
	return ok
}

func TestHandleKey_RoutesByScreenState(t *testing.T) {
	editing := func(m model) model {
		m.edit = beginEdit(editURL, "url", "")
		return m
	}
	cases := []struct {
		name  string
		setup func(model) model
		key   tea.KeyMsg
		check func(*testing.T, model, tea.Cmd)
	}{
		{
			name: "? expands the help",
			key:  keyRunes("?"),
			check: func(t *testing.T, m model, _ tea.Cmd) {
				if !m.help.ShowAll {
					t.Error("help stayed collapsed")
				}
			},
		},
		{
			name:  "an open editor takes ? as text",
			setup: editing,
			key:   keyRunes("?"),
			check: func(t *testing.T, m model, _ tea.Cmd) {
				if m.help.ShowAll || m.edit.input.Value() != "?" {
					t.Errorf("help=%v field=%q, want the rune typed into the field", m.help.ShowAll, m.edit.input.Value())
				}
			},
		},
		{
			name:  "an open editor takes q as text",
			setup: editing,
			key:   keyRunes("q"),
			check: func(t *testing.T, m model, cmd tea.Cmd) {
				if quitMsg(cmd) || m.edit.input.Value() != "q" {
					t.Errorf("field=%q quit=%v, want the rune typed into the field", m.edit.input.Value(), quitMsg(cmd))
				}
			},
		},
		{
			name:  "ctrl+c still quits from an open editor",
			setup: editing,
			key:   tea.KeyMsg{Type: tea.KeyCtrlC},
			check: func(t *testing.T, _ model, cmd tea.Cmd) {
				if !quitMsg(cmd) {
					t.Error("ctrl+c did not quit")
				}
			},
		},
		{
			name: "v rechecks the connection",
			key:  keyRunes("v"),
			check: func(t *testing.T, m model, _ tea.Cmd) {
				if m.prepareSeq != 2 || m.prepareStatus != "checking" {
					t.Errorf("seq=%d status=%q, want a second attempt", m.prepareSeq, m.prepareStatus)
				}
			},
		},
		{
			name: "space activates the selected row",
			setup: func(m model) model {
				m.section, m.row = sectionServers, 0
				m.cfg.BaseURL = "http://elsewhere.invalid:9999"
				return m
			},
			key: keyRunes(" "),
			check: func(t *testing.T, m model, _ tea.Cmd) {
				if m.cfg.BaseURL != serverPresets[0].url {
					t.Errorf("BaseURL = %q, want %q", m.cfg.BaseURL, serverPresets[0].url)
				}
				if m.prepareSeq != 2 {
					t.Errorf("seq = %d, want the new server checked", m.prepareSeq)
				}
			},
		},
		{
			name: "r reaches the runner",
			setup: func(m model) model {
				m.cfg.Stages = goclient.StageSet{}
				return m
			},
			key: keyRunes("r"),
			check: func(t *testing.T, m model, _ tea.Cmd) {
				if !strings.Contains(m.notice, "Enable at least one stage") {
					t.Errorf("notice = %q, want the runner's refusal", m.notice)
				}
			},
		},
		{
			name: "quit outranks the cancel prompt",
			setup: func(m model) model {
				m.mode, m.cancelPrompt = modeRun, true
				return m
			},
			key: keyRunes("q"),
			check: func(t *testing.T, _ model, cmd tea.Cmd) {
				if !quitMsg(cmd) {
					t.Error("q during the cancel prompt did not quit")
				}
			},
		},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			m := newModel(goclient.DefaultConfig())
			if c.setup != nil {
				m = c.setup(m)
			}
			next, cmd := modelAndCmd(m.handleKey(c.key))
			c.check(t, next, cmd)
		})
	}
}

func TestHelpFooterListsEveryBindingTheScreenAccepts(t *testing.T) {
	cases := []struct {
		name  string
		model func(model) model
	}{
		{"configure", func(m model) model { return m }},
		{"editing", func(m model) model {
			m.edit = beginEdit(editURL, "url", m.cfg.BaseURL)
			return m
		}},
		{"running", func(m model) model {
			m.mode = modeRun
			return m
		}},
		{"cancel prompt", func(m model) model {
			m.mode, m.cancelPrompt = modeRun, true
			return m
		}},
		{"complete", func(m model) model {
			m.mode, m.complete = modeRun, true
			return m
		}},
		{"awaiting approval", func(m model) model {
			m.auth = &goclient.PendingAuthorization{Code: "ABCDE"}
			return m
		}},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			m := c.model(newModel(goclient.DefaultConfig()))
			m.width = 200
			footer := ansiPattern.ReplaceAllString(m.helpView(), "")
			for _, b := range m.ShortHelp() {
				if !strings.Contains(footer, b.Help().Key) {
					t.Errorf("footer %q omits %q", footer, b.Help().Key)
				}
			}
		})
	}
}

func TestActivate_ServerCases(t *testing.T) {
	cases := []struct {
		name       string
		row        int
		baseURL    string
		wantPreset bool
	}{
		{"ServerPreset", 0, "http://elsewhere.invalid:9999", true},
		{"ServerCustomStartsEdit", len(serverPresets), "", false},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			m := newModel(goclient.DefaultConfig())
			m.section, m.row = sectionServers, c.row
			if c.baseURL != "" {
				m.cfg.BaseURL = c.baseURL
			}
			m, _ = modelAndCmd(m.activate())
			if c.wantPreset {
				if m.cfg.BaseURL != serverPresets[0].url {
					t.Errorf("BaseURL = %q, want %q", m.cfg.BaseURL, serverPresets[0].url)
				}
				if !strings.Contains(m.notice, serverPresets[0].name) {
					t.Errorf("notice = %q, want mention of %q", m.notice, serverPresets[0].name)
				}
			} else if m.edit.kind != editURL || m.edit.input.Value() != m.cfg.BaseURL {
				t.Errorf("custom edit = %+v value=%q, want URL editor for %q", m.edit, m.edit.input.Value(), m.cfg.BaseURL)
			}
		})
	}
}

func TestActivate_StagesToggle(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.section = sectionRunSetup
	cfg := m.cfg

	getters := []func(goclient.Config) bool{
		func(c goclient.Config) bool { return c.Stages.Latency },
		func(c goclient.Config) bool { return c.Stages.Download },
		func(c goclient.Config) bool { return c.Stages.Upload },
		func(c goclient.Config) bool { return c.Stages.Bidirectional },
		func(c goclient.Config) bool { return c.LoadedLatency },
	}
	for row, get := range getters {
		before := get(cfg)
		m.row = row
		m, _ = modelAndCmd(m.activate())
		if get(m.cfg) == before {
			t.Errorf("row %d did not toggle: before=%v after=%v", row, before, get(m.cfg))
		}
		cfg = m.cfg
	}
}

func TestActivate_EditorCases(t *testing.T) {
	cases := []struct {
		name      string
		section   section
		row       int
		kind      editKind
		field     string
		wantValue func(model) string
	}{
		{"warmup duration", sectionTiming, 0, editDuration, "warmup", func(m model) string { return m.durationValue(0) }},
		{"latency duration", sectionTiming, 1, editDuration, "latency", func(m model) string { return m.durationValue(1) }},
		{"download duration", sectionTiming, 2, editDuration, "download", func(m model) string { return m.durationValue(2) }},
		{"upload duration", sectionTiming, 3, editDuration, "upload", func(m model) string { return m.durationValue(3) }},
		{"bidirectional duration", sectionTiming, 4, editDuration, "bidirectional", func(m model) string { return m.durationValue(4) }},
		{"ping duration", sectionTiming, 5, editDuration, "ping", func(m model) string { return m.durationValue(5) }},
		{"automatic streams", sectionConnections, rowAutoStreams, editInt, "auto-streams", nil},
		{"forced streams", sectionConnections, rowStreams, editInt, "streams", nil},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			m := newModel(goclient.DefaultConfig())
			m.section, m.row = c.section, c.row
			edited, _ := modelAndCmd(m.activate())
			if edited.edit.kind != c.kind || edited.edit.field != c.field {
				t.Errorf("edit = %+v, want %v %q", edited.edit, c.kind, c.field)
			}
			if c.wantValue != nil && edited.edit.input.Value() != c.wantValue(m) {
				t.Errorf("edit value = %q, want %q", edited.edit.input.Value(), c.wantValue(m))
			}
		})
	}
}

func TestNetworkView_AutoH1MaxIsMarkedUnusedOverWebTransport(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.cfg.ThroughputTransport = wire.TransportFetchStream
	plain := ansiPattern.ReplaceAllString(m.networkView(80), "")
	if !strings.Contains(plain, "Auto H1 max") || strings.Contains(plain, "unused over WebTransport") {
		t.Errorf("fetch-stream rows carry the WebTransport annotation:\n%s", plain)
	}

	m.cfg.ThroughputTransport = wire.TransportWebTransport
	if plain = ansiPattern.ReplaceAllString(m.networkView(80), ""); !strings.Contains(plain, "unused over WebTransport") {
		t.Errorf("the ceiling row is not annotated over WebTransport:\n%s", plain)
	}
}

func TestNetworkViewEditorsRenderOnTheirOwnRows(t *testing.T) {
	cases := []struct{ field, label, other string }{
		{"auto-streams", "Auto H1 max", "Streams"},
		{"streams", "Streams", "Auto H1 max"},
	}
	for _, c := range cases {
		t.Run(c.field, func(t *testing.T) {
			m := newModel(goclient.DefaultConfig())
			m.section, m.row = sectionConnections, 0
			m.edit = beginEdit(editInt, c.field, "7")

			var editing []string
			for line := range strings.SplitSeq(ansiPattern.ReplaceAllString(m.networkView(120), ""), "\n") {
				if strings.Contains(line, "editing") {
					editing = append(editing, strings.TrimSpace(line))
				}
			}
			if len(editing) != 1 {
				t.Fatalf("editing %q marked %d rows as being edited, want exactly 1:\n%s", c.field, len(editing), strings.Join(editing, "\n"))
			}
			if !strings.Contains(editing[0], c.label) {
				t.Errorf("editing %q drew the field on %q, want the %q row", c.field, editing[0], c.label)
			}
			if strings.Contains(editing[0], c.other) {
				t.Errorf("editing %q drew over the %q row: %q", c.field, c.other, editing[0])
			}
		})
	}
}

func TestActivate_ThroughputProtocolFollowsTheSelectedPath(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.section = sectionConnections
	m.discovery = &wire.Preflight{Capabilities: wire.Capabilities{
		ThroughputTargets: []wire.ThroughputTarget{
			{Origin: "https://fixed.example", Transport: wire.TransportFetchStream, Protocol: "http3", TLS: true},
			{Origin: "https://proxy.example", Transport: wire.TransportFetchStream, Protocol: protocolNegotiated, TLS: true},
		},
	}}

	cases := []struct {
		name, target, protocol string
		row                    int
		checkFixed             bool
	}{
		{"automatic protocol", "", "http1", rowThroughputProtocol, false},
		{"fixed path", "https://fixed.example", "auto", rowThroughputPath, false},
		{"fixed protocol", "", "auto", rowThroughputProtocol, true},
		{"negotiated path", "", "", rowThroughputPath, false},
		{"negotiated protocol", "", "http1", rowThroughputProtocol, false},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			m.row = c.row
			m, _ = modelAndCmd(m.activate())
			if c.target != "" && m.cfg.ThroughputTarget != c.target {
				t.Fatalf("target = %q, want %q", m.cfg.ThroughputTarget, c.target)
			}
			if c.protocol != "" && m.cfg.ThroughputProtocol != c.protocol {
				t.Fatalf("protocol = %q, want %q", m.cfg.ThroughputProtocol, c.protocol)
			}
			if !c.checkFixed {
				return
			}
			if !strings.Contains(m.notice, "HTTP/3") {
				t.Errorf("refusal notice = %q, want it to name what the path serves", m.notice)
			}
			plain := ansiPattern.ReplaceAllString(m.networkView(120), "")
			if !strings.Contains(plain, "fixed by this path") {
				t.Errorf("the version row does not read as inert:\n%s", plain)
			}
		})
	}
}

func TestActivate_NetworkCases(t *testing.T) {
	cases := []struct {
		name  string
		row   int
		reset bool
	}{
		{"NetworkTLSToggle", rowSkipTLS, false},
		{"NetworkReset", rowReset, true},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			m := newModel(goclient.DefaultConfig())
			m.section, m.row = sectionConnections, c.row
			before := m.cfg.InsecureSkipTLSVerify
			if c.reset {
				m.cfg.TransferStreams.Forced = 99
				m.cfg.BaseURL = "http://changed.example"
			}
			m, _ = modelAndCmd(m.activate())
			if c.reset {
				if want := goclient.DefaultConfig(); m.cfg != want {
					t.Errorf("cfg after reset = %+v, want default %+v", m.cfg, want)
				}
			} else if m.cfg.InsecureSkipTLSVerify == before {
				t.Error("InsecureSkipTLSVerify was not toggled")
			}
		})
	}
}

func TestAuthTokenResultIsBoundToCurrentServer(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.cfg.BaseURL = "https://new.example"
	m, _ = modelAndCmd(m.Update(authTokenMsg{seq: m.prepareSeq, token: "secret", origin: "https://old.example"}))
	if m.cfg.AuthToken != "" || m.cfg.AuthOrigin != "" {
		t.Fatal("stale authorization result was retained")
	}

	m, _ = modelAndCmd(m.Update(authTokenMsg{seq: m.prepareSeq, token: "secret", origin: "https://new.example"}))
	if m.cfg.AuthToken != "secret" || m.cfg.AuthOrigin != "https://new.example" {
		t.Fatal("matching authorization result was not retained")
	}
}

func TestChangingServerClearsAuthorization(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.cfg.BaseURL = "https://old.example"
	m.cfg.AuthToken = "secret"
	m.cfg.AuthOrigin = "https://old.example"
	m.edit = beginEdit(editURL, "url", "https://new.example")
	m.commitEdit()
	if m.cfg.AuthToken != "" || m.cfg.AuthOrigin != "" {
		t.Fatal("authorization was retained after editing the server")
	}
}

func TestHandleEditKey_TypeBackspaceCommitCancel(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.edit = beginEdit(editURL, "url", "")

	m, _ = modelAndCmd(m.handleKey(keyRunes("h")))
	m, _ = modelAndCmd(m.handleKey(keyRunes("i")))
	if m.edit.input.Value() != "hi" {
		t.Fatalf("edit value after typing = %q, want %q", m.edit.input.Value(), "hi")
	}

	m, _ = modelAndCmd(m.handleKey(tea.KeyMsg{Type: tea.KeyBackspace}))
	if m.edit.input.Value() != "h" {
		t.Fatalf("edit value after backspace = %q, want %q", m.edit.input.Value(), "h")
	}

	m, _ = modelAndCmd(m.handleKey(tea.KeyMsg{Type: tea.KeyEsc}))
	if m.edit.kind != editNone {
		t.Errorf("edit.kind after esc = %v, want editNone", m.edit.kind)
	}
	if !strings.Contains(m.notice, "canceled") {
		t.Errorf("notice after esc = %q, want mention of canceled", m.notice)
	}
}

func TestHandleEditKey_BackspaceOnEmptyIsNoop(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.edit = beginEdit(editURL, "", "")
	m, _ = modelAndCmd(m.handleKey(tea.KeyMsg{Type: tea.KeyBackspace}))
	if m.edit.input.Value() != "" {
		t.Errorf("edit value after backspace on empty = %q, want empty", m.edit.input.Value())
	}
}

func TestUpdate_PasteFillsTheURLField(t *testing.T) {
	const url = "https://meter.example.com:7247"
	m := newModel(goclient.DefaultConfig())
	m.section = sectionServers
	m.row = len(serverPresets) // the "Custom URL" row

	m, _ = modelAndCmd(m.Update(keyPaste(url)))
	if m.edit.kind != editURL {
		t.Fatalf("edit.kind after pasting on the selected Custom URL row = %v, want editURL", m.edit.kind)
	}
	if m.edit.input.Value() != url {
		t.Fatalf("field after paste = %q, want %q", m.edit.input.Value(), url)
	}

	m, _ = modelAndCmd(m.Update(keyPaste("/base")))
	if want := url + "/base"; m.edit.input.Value() != want {
		t.Fatalf("field after a second paste = %q, want %q", m.edit.input.Value(), want)
	}

	m, _ = modelAndCmd(m.Update(tea.KeyMsg{Type: tea.KeyEnter}))
	if want := url + "/base"; m.cfg.BaseURL != want {
		t.Errorf("BaseURL after committing the pasted URL = %q, want %q", m.cfg.BaseURL, want)
	}
}

func TestUpdate_PasteLandsAtTheCursorOfAnOpenField(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.edit = beginEdit(editURL, "url", "meter.example:7247")

	m, _ = modelAndCmd(m.Update(tea.KeyMsg{Type: tea.KeyHome}))
	m, _ = modelAndCmd(m.Update(keyPaste("https://")))
	if want := "https://meter.example:7247"; m.edit.input.Value() != want {
		t.Errorf("field after pasting at the line start = %q, want %q", m.edit.input.Value(), want)
	}
}

func TestHandleKey_TypingOnTheCustomURLRow(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.section = sectionServers
	m.row = len(serverPresets)

	for _, seed := range []string{"h", "r", "v", "j", "k", "q", "?"} {
		edited, _ := modelAndCmd(m.handleKey(keyRunes(seed)))
		if edited.edit.kind != editURL || edited.edit.input.Value() != seed {
			t.Errorf("typing %q on the Custom URL row gave kind=%v value=%q, want editURL seeded with it", seed, edited.edit.kind, edited.edit.input.Value())
		}
		if edited.mode != modeConfigure {
			t.Errorf("typing %q left configure mode", seed)
		}
	}

	// ctrl+c is not a rune and still quits.
	_, cmd := m.handleKey(tea.KeyMsg{Type: tea.KeyCtrlC})
	if cmd == nil {
		t.Error("ctrl+c on the Custom URL row returned no quit cmd")
	}

	m.row = 0 // a preset row is not a text field
	next, _ := modelAndCmd(m.handleKey(keyRunes("h")))
	if kept := next; kept.edit.kind != editNone {
		t.Errorf("typing on a preset row started edit %v, want none", kept.edit.kind)
	}
}

func TestCommitEdit_RejectsANonURL(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	before := m.cfg.BaseURL
	for _, raw := range []string{"not a url", "ftp://meter.example", "http://"} {
		m.edit = beginEdit(editURL, "url", raw)
		m.commitEdit()
		if m.edit.kind != editURL || m.edit.err == "" {
			t.Errorf("committing %q: kind=%v err=%q, want the field kept open with a reason", raw, m.edit.kind, m.edit.err)
		}
		if m.cfg.BaseURL != before {
			t.Errorf("committing %q changed BaseURL to %q", raw, m.cfg.BaseURL)
		}
	}
}

func TestCommitEdit_URLIsTakenAsTyped(t *testing.T) {
	cases := map[string]string{
		"meter.example:7247":       "http://meter.example:7247",
		"meter.example":            "http://meter.example",
		"https://meter.example":    "https://meter.example",
		"http://meter.example:900": "http://meter.example:900",
	}
	for typed, want := range cases {
		m := newModel(goclient.DefaultConfig())
		m.edit = beginEdit(editURL, "url", typed)
		m.commitEdit()
		if m.edit.kind != editNone || m.cfg.BaseURL != want {
			t.Errorf("%q committed as %q (kind=%v), want %q", typed, m.cfg.BaseURL, m.edit.kind, want)
		}
	}
}

func TestCommitEdit_BareNumberIsSeconds(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.edit = beginEdit(editDuration, "download", "12")
	m.commitEdit()
	if m.edit.kind != editNone || m.cfg.DownloadDuration != 12*time.Second {
		t.Errorf("kind=%v duration=%v, want a bare 12 committed as 12s", m.edit.kind, m.cfg.DownloadDuration)
	}
	m.edit = beginEdit(editDuration, "download", "0.5")
	m.commitEdit()
	if m.cfg.DownloadDuration != 500*time.Millisecond {
		t.Errorf("duration=%v, want a bare 0.5 committed as 500ms", m.cfg.DownloadDuration)
	}
}

func TestCommitEdit_PingBoundFollowsTheLatencyBus(t *testing.T) {
	wide := (goclient.MaxPingInterval + 5*time.Second).String()

	m := newModel(goclient.DefaultConfig())
	m.cfg.LatencyTransport = wire.TransportWebSocket
	m.edit = beginEdit(editDuration, "ping", wide)
	m.commitEdit()
	if m.edit.kind != editNone || m.cfg.PingInterval.String() != wide {
		t.Fatalf("a %s cadence over the WebSocket bus was refused: kind=%v err=%q interval=%v", wide, m.edit.kind, m.edit.err, m.cfg.PingInterval)
	}

	m.cfg.LatencyTransport = wire.TransportWebTransport
	m.edit = beginEdit(editDuration, "ping", wide)
	m.commitEdit()
	if m.edit.err == "" || !strings.Contains(m.edit.err, goclient.MaxPingInterval.String()) {
		t.Fatalf("a %s cadence over the datagram bus gave err=%q, want one naming the %v bound", wide, m.edit.err, goclient.MaxPingInterval)
	}

	m.cfg.LatencyTransport = "auto"
	m.edit = beginEdit(editDuration, "ping", wide)
	m.commitEdit()
	if m.edit.kind != editNone || m.cfg.PingInterval.String() != wide {
		t.Fatalf("an automatic %s cadence was rejected before transport verification: kind=%v err=%q interval=%v", wide, m.edit.kind, m.edit.err, m.cfg.PingInterval)
	}
}

func TestUpdate_StaleRunMessagesAreDropped(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.mode = modeRun
	m.runSeq = 2
	m.events = make(chan goclient.Event)

	got, cmd := modelAndCmd(m.Update(eventsMsg{seq: 1, events: []goclient.Event{{Kind: goclient.EventStage, Stage: "download"}}}))
	mm := got
	if mm.stage != "" || cmd != nil {
		t.Errorf("stale eventsMsg applied: stage=%q cmd=%v, want dropped", mm.stage, cmd)
	}

	got, _ = modelAndCmd(m.Update(doneMsg{seq: 1, err: errors.New("boom")}))
	mm = got
	if mm.complete || mm.err != nil {
		t.Errorf("stale doneMsg applied: complete=%v err=%v, want dropped", mm.complete, mm.err)
	}

	got, _ = modelAndCmd(m.Update(doneMsg{seq: 2, err: nil}))
	if mm = got; !mm.complete {
		t.Error("current-run doneMsg was not applied")
	}
}

func TestHandleEditKey_CursorMovementAndRuneSafeDelete(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.edit = beginEdit(editURL, "url", "hé")

	m, _ = modelAndCmd(m.handleKey(tea.KeyMsg{Type: tea.KeyBackspace}))
	if m.edit.input.Value() != "h" {
		t.Fatalf("value after backspacing a multi-byte rune = %q, want %q", m.edit.input.Value(), "h")
	}

	m, _ = modelAndCmd(m.handleKey(tea.KeyMsg{Type: tea.KeyHome}))
	m, _ = modelAndCmd(m.handleKey(keyRunes("x")))
	if m.edit.input.Value() != "xh" {
		t.Errorf("value after typing at the line start = %q, want %q", m.edit.input.Value(), "xh")
	}
}

func TestCommitEdit_RejectionKeepsTheFieldOpen(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.edit = beginEdit(editDuration, "download", "nope")
	m.commitEdit()

	if m.edit.kind != editDuration || m.edit.input.Value() != "nope" {
		t.Fatalf("rejected edit = %v/%q, want the field still open on the typed text", m.edit.kind, m.edit.input.Value())
	}
	if m.edit.err == "" {
		t.Error("rejected edit has no inline error")
	}

	m, _ = modelAndCmd(m.handleKey(keyRunes("!")))
	if m.edit.err != "" {
		t.Errorf("inline error %q survived further typing", m.edit.err)
	}
}

func TestCommitEdit_URL(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.edit = beginEdit(editURL, "url", "  http://example.test:9  ")
	m.commitEdit()
	if m.cfg.BaseURL != "http://example.test:9" {
		t.Errorf("BaseURL after commit = %q, want trimmed URL", m.cfg.BaseURL)
	}
	if m.edit.kind != editNone {
		t.Error("edit state should be cleared after commit")
	}

	prevURL := m.cfg.BaseURL
	m.edit = beginEdit(editURL, "url", "   ")
	m.commitEdit()
	if m.cfg.BaseURL != prevURL {
		t.Errorf("BaseURL should be unchanged on empty commit, got %q", m.cfg.BaseURL)
	}
	if !strings.Contains(m.notice, "cannot be empty") {
		t.Errorf("notice = %q, want mention of empty URL", m.notice)
	}
}

func TestCommitEdit_Duration(t *testing.T) {
	cases := []struct {
		field   string
		value   string
		wantErr bool
		check   func(goclient.Config) time.Duration
	}{
		{"warmup", "1500ms", false, func(c goclient.Config) time.Duration { return c.Warmup }},
		{"warmup", "0s", false, func(c goclient.Config) time.Duration { return c.Warmup }}, // warmup allows zero
		{"latency", "5s", false, func(c goclient.Config) time.Duration { return c.LatencyDuration }},
		{"latency", "0s", true, func(c goclient.Config) time.Duration { return c.LatencyDuration }},
		{"download", "0s", true, func(c goclient.Config) time.Duration { return c.DownloadDuration }},
		{"upload", "0s", true, func(c goclient.Config) time.Duration { return c.UploadDuration }},
		{"bidirectional", "0s", true, func(c goclient.Config) time.Duration { return c.BidirectionalDuration }},
		{"ping", "0s", true, func(c goclient.Config) time.Duration { return c.PingInterval }},
		{"ping", "not-a-duration", true, func(c goclient.Config) time.Duration { return c.PingInterval }},
		{"ping", "-5s", true, func(c goclient.Config) time.Duration { return c.PingInterval }},
		// Automatic transport defers its bus-specific upper bound to Prepare.
		{"ping", "45s", false, func(c goclient.Config) time.Duration { return c.PingInterval }},
		{"ping", goclient.MaxPingInterval.String(), false, func(c goclient.Config) time.Duration { return c.PingInterval }},
	}
	for _, c := range cases {
		t.Run(c.field+"_"+c.value, func(t *testing.T) {
			m := newModel(goclient.DefaultConfig())
			before := c.check(m.cfg)
			m.edit = beginEdit(editDuration, c.field, c.value)
			m.commitEdit()
			got := c.check(m.cfg)
			if c.wantErr {
				if got != before {
					t.Errorf("field %s changed to %v despite invalid input %q", c.field, got, c.value)
				}
				if m.notice == "" {
					t.Error("expected an error notice for invalid input")
				}
			} else {
				want, _ := time.ParseDuration(c.value)
				if got != want {
					t.Errorf("field %s = %v, want %v", c.field, got, want)
				}
			}
		})
	}
}

func TestCommitEdit_Int(t *testing.T) {
	cases := []struct {
		value   string
		wantErr bool
		want    int
	}{
		{"8", false, 8},
		{"1", false, 1},
		{"128", false, 128},
		{"0", false, 0},
		{"129", true, 0},
		{"abc", true, 0},
		{"", true, 0},
	}
	for _, c := range cases {
		t.Run(c.value, func(t *testing.T) {
			m := newModel(goclient.DefaultConfig())
			before := m.cfg.TransferStreams.Forced
			m.edit = beginEdit(editInt, "", c.value)
			m.commitEdit()
			if c.wantErr {
				if m.cfg.TransferStreams.Forced != before {
					t.Errorf("forced streams changed to %d despite invalid input %q", m.cfg.TransferStreams.Forced, c.value)
				}
			} else if m.cfg.TransferStreams.Forced != c.want {
				t.Errorf("forced streams = %d, want %d", m.cfg.TransferStreams.Forced, c.want)
			}
		})
	}
}

func TestCommitEdit_AutomaticStreams(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.edit = beginEdit(editInt, "auto-streams", "1")
	m.commitEdit()
	if m.cfg.TransferStreams.AutomaticMax != 1 {
		t.Fatalf("automatic max = %d, want 1", m.cfg.TransferStreams.AutomaticMax)
	}
	m.edit = beginEdit(editInt, "auto-streams", "0")
	m.commitEdit()
	if m.cfg.TransferStreams.AutomaticMax != 1 {
		t.Fatalf("invalid automatic max changed to %d", m.cfg.TransferStreams.AutomaticMax)
	}
}

// A recheck costs a round trip, so a commit that leaves the configuration identical starts none.
func TestHandleEditKey_ApplyRechecksOnlyWhatChanged(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	seq := m.prepareSeq

	m.edit = beginEdit(editDuration, "download", "nope")
	m, _ = modelAndCmd(m.handleKey(tea.KeyMsg{Type: tea.KeyEnter}))
	if m.edit.kind != editDuration || m.prepareSeq != seq {
		t.Fatalf("rejected commit: kind=%v seq=%d, want the field open and no recheck", m.edit.kind, m.prepareSeq)
	}

	m.edit = beginEdit(editDuration, "download", m.durationValue(2))
	m, _ = modelAndCmd(m.handleKey(tea.KeyMsg{Type: tea.KeyEnter}))
	if m.edit.kind != editNone || m.prepareSeq != seq {
		t.Fatalf("commit of the current value: kind=%v seq=%d, want the field closed and no recheck", m.edit.kind, m.prepareSeq)
	}

	m.edit = beginEdit(editDuration, "download", "12s")
	m, _ = modelAndCmd(m.handleKey(tea.KeyMsg{Type: tea.KeyEnter}))
	if m.cfg.DownloadDuration != 12*time.Second {
		t.Errorf("download duration = %v, want 12s", m.cfg.DownloadDuration)
	}
	if m.prepareSeq != seq+1 || m.prepareStatus != "checking" {
		t.Errorf("seq=%d status=%q, want the changed configuration rechecked", m.prepareSeq, m.prepareStatus)
	}
}

func TestCommitEdit_RetypingTheSameURLKeepsAuthorization(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.cfg.AuthToken, m.cfg.AuthOrigin = "grant", m.cfg.BaseURL
	m.edit = beginEdit(editURL, "url", "  "+m.cfg.BaseURL+"  ")
	m.commitEdit()
	if m.cfg.AuthToken != "grant" {
		t.Errorf("authorization dropped by retyping the same URL, token = %q", m.cfg.AuthToken)
	}
}

func TestHandleKey_RunMode_CancelTakesTwoEscapes(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.mode = modeRun
	canceled := false
	m.cancel = func() { canceled = true }

	m, _ = modelAndCmd(m.handleKey(tea.KeyMsg{Type: tea.KeyEsc}))
	if !m.cancelPrompt || canceled {
		t.Fatalf("first esc: cancelPrompt=%v canceled=%v, want a prompt and no cancel", m.cancelPrompt, canceled)
	}

	m, _ = modelAndCmd(m.handleKey(keyRunes("j")))
	if m.cancelPrompt || canceled {
		t.Fatalf("other key: cancelPrompt=%v canceled=%v, want the prompt dropped and the run kept", m.cancelPrompt, canceled)
	}

	m, _ = modelAndCmd(m.handleKey(tea.KeyMsg{Type: tea.KeyEsc}))
	m, _ = modelAndCmd(m.handleKey(tea.KeyMsg{Type: tea.KeyEsc}))
	if !canceled || m.status != "canceling" {
		t.Errorf("second esc: canceled=%v status=%q, want the run canceled", canceled, m.status)
	}
}

func TestHandleKey_RunMode_EscapeReturnsToSetupWhenComplete(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.mode = modeRun
	m.complete = true

	m, _ = modelAndCmd(m.handleKey(tea.KeyMsg{Type: tea.KeyEsc}))
	if m.mode != modeConfigure || m.section != sectionRun || m.row != 0 {
		t.Errorf("after esc on a complete run: mode=%v section=%v row=%d", m.mode, m.section, m.row)
	}
}

func TestHandleKey_RunMode_RIgnoredUntilComplete(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.mode = modeRun
	m.complete = false
	next, cmd := m.handleKey(keyRunes("r"))
	m = next.(model)
	if m.mode != modeRun {
		t.Errorf("mode changed to %v while run in progress, want modeRun unchanged", m.mode)
	}
	if cmd != nil {
		t.Error("expected nil cmd while run in progress")
	}
}

func TestStartRun_NoStagesSelected(t *testing.T) {
	cfg := goclient.DefaultConfig()
	cfg.Stages = goclient.StageSet{}
	m := newModel(cfg)

	next, cmd := m.startRun()
	if next.mode != modeConfigure {
		t.Errorf("mode = %v, want modeConfigure", next.mode)
	}
	if next.section != sectionRunSetup || next.row != 0 {
		t.Errorf("section=%v row=%d, want sectionRunSetup/0", next.section, next.row)
	}
	if !strings.Contains(next.notice, "Enable at least one stage") {
		t.Errorf("notice = %q, want stage requirement message", next.notice)
	}
	if cmd != nil {
		t.Error("expected nil cmd when no stages selected")
	}
}

func TestStartRun_LaunchesRun(t *testing.T) {
	cfg := goclient.DefaultConfig()
	cfg.Stages = goclient.StageSet{Latency: true}
	cfg.BaseURL = "http://127.0.0.1:1" // reserved port: connection fails immediately, no real network needed
	m := newModel(cfg)

	next, cmd := m.startRun()
	if next.mode != modeRun {
		t.Errorf("mode = %v, want modeRun", next.mode)
	}
	if next.status != "connecting" {
		t.Errorf("status = %q, want connecting", next.status)
	}
	if next.complete {
		t.Error("complete should be false right after starting a run")
	}
	if cmd == nil {
		t.Error("expected a non-nil cmd batching waitEvent/waitDone")
	}

	if next.cancel != nil {
		next.cancel()
	}
	select {
	case <-next.done:
	case <-time.After(5 * time.Second):
		t.Fatal("run goroutine did not finish after cancel")
	}
}

func TestApply_Events(t *testing.T) {
	m := newModel(goclient.DefaultConfig())

	m.apply(goclient.Event{Kind: goclient.EventStage, Stage: "download", Message: "measure"})
	if m.stage != "download" || m.status != "measure" {
		t.Errorf("after EventStage: stage=%q status=%q", m.stage, m.status)
	}

	m.apply(goclient.Event{Kind: goclient.EventThroughput, Direction: goclient.Down, Throughput: goclient.ThroughputSample{BytesPerSec: 100}})
	if m.rates[goclient.Down].BytesPerSec != 100 || m.peaks[goclient.Down] != 100 {
		t.Errorf("after first throughput: rate=%v peak=%v", m.rates[goclient.Down].BytesPerSec, m.peaks[goclient.Down])
	}
	m.apply(goclient.Event{Kind: goclient.EventThroughput, Direction: goclient.Down, Throughput: goclient.ThroughputSample{BytesPerSec: 50}})
	if m.rates[goclient.Down].BytesPerSec != 50 {
		t.Errorf("rate should track the latest sample, got %v", m.rates[goclient.Down].BytesPerSec)
	}
	if m.peaks[goclient.Down] != 100 {
		t.Errorf("peak should not drop below prior max, got %v", m.peaks[goclient.Down])
	}
	m.apply(goclient.Event{Kind: goclient.EventThroughput, Direction: goclient.Down, Throughput: goclient.ThroughputSample{BytesPerSec: 200}})
	if m.peaks[goclient.Down] != 200 {
		t.Errorf("peak should rise to new max, got %v", m.peaks[goclient.Down])
	}

	m.apply(goclient.Event{Kind: goclient.EventLatency, Latency: goclient.LatencySample{RTT: 5 * time.Millisecond}})
	if m.latency.RTT != 5*time.Millisecond {
		t.Errorf("latency.RTT = %v, want 5ms", m.latency.RTT)
	}

	m.apply(goclient.Event{Kind: goclient.EventResult, Result: nil})
	if len(m.results) != 0 {
		t.Errorf("nil Result should not be appended, got %d results", len(m.results))
	}
	m.apply(goclient.Event{Kind: goclient.EventResult, Result: &goclient.Result{Stage: "download"}})
	if len(m.results) != 1 || m.results[0].Stage != "download" {
		t.Errorf("results = %+v, want single download result", m.results)
	}

	m.apply(goclient.Event{Kind: goclient.EventComplete})
	if !m.complete || m.status != "complete" {
		t.Errorf("after EventComplete: complete=%v status=%q", m.complete, m.status)
	}

	boom := errors.New("boom")
	m.apply(goclient.Event{Kind: goclient.EventError, Err: boom})
	if m.err != boom || m.status != "error" {
		t.Errorf("after EventError: err=%v status=%q", m.err, m.status)
	}

	m2 := newModel(goclient.DefaultConfig())
	pf := &wire.Preflight{Server: wire.ServerInfo{Name: "srv", Location: "ams"}}
	m2.apply(goclient.Event{Kind: goclient.EventPreflight, Preflight: pf})
	if m2.status != "connected" || !strings.Contains(m2.server, "srv") {
		t.Errorf("after EventPreflight: status=%q server=%q", m2.status, m2.server)
	}
}

func TestApply_DuplicateAndOutOfOrderEvents(t *testing.T) {
	m := newModel(goclient.DefaultConfig())

	m.apply(goclient.Event{Kind: goclient.EventComplete})
	if !m.complete {
		t.Fatal("expected complete after EventComplete")
	}

	m.apply(goclient.Event{Kind: goclient.EventResult, Result: &goclient.Result{Stage: "download"}})
	if len(m.results) != 1 {
		t.Errorf("late-arriving Result after Complete should still be recorded, got %d results", len(m.results))
	}

	// A duplicate Complete is a no-op.
	m.apply(goclient.Event{Kind: goclient.EventComplete})
	if !m.complete || m.status != "complete" {
		t.Errorf("duplicate EventComplete changed state unexpectedly: complete=%v status=%q", m.complete, m.status)
	}

	// A late Error still overwrites status/err; apply has no "already done" guard against post-completion events.
	boom := errors.New("late boom")
	m.apply(goclient.Event{Kind: goclient.EventError, Err: boom})
	if m.err != boom || m.status != "error" {
		t.Errorf("late EventError not applied: err=%v status=%q", m.err, m.status)
	}

	// Interleaved throughput samples for both directions must not clobber each other's rate or peak.
	m2 := newModel(goclient.DefaultConfig())
	m2.apply(goclient.Event{Kind: goclient.EventThroughput, Direction: goclient.Down, Throughput: goclient.ThroughputSample{BytesPerSec: 100}})
	m2.apply(goclient.Event{Kind: goclient.EventThroughput, Direction: goclient.Up, Throughput: goclient.ThroughputSample{BytesPerSec: 40}})
	m2.apply(goclient.Event{Kind: goclient.EventThroughput, Direction: goclient.Down, Throughput: goclient.ThroughputSample{BytesPerSec: 30}})
	m2.apply(goclient.Event{Kind: goclient.EventThroughput, Direction: goclient.Up, Throughput: goclient.ThroughputSample{BytesPerSec: 90}})
	if m2.peaks[goclient.Down] != 100 {
		t.Errorf("down peak = %v, want 100 (unaffected by interleaved up samples)", m2.peaks[goclient.Down])
	}
	if m2.peaks[goclient.Up] != 90 {
		t.Errorf("up peak = %v, want 90", m2.peaks[goclient.Up])
	}
	if m2.rates[goclient.Down].BytesPerSec != 30 || m2.rates[goclient.Up].BytesPerSec != 90 {
		t.Errorf("rates = %+v, want the latest per-direction sample for each", m2.rates)
	}
}

func TestUpdate_DrainsEventsAfterComplete(t *testing.T) {
	events := make(chan goclient.Event, 1)
	events <- goclient.Event{
		Kind:   goclient.EventResult,
		Stage:  "bidirectional",
		Result: &goclient.Result{Stage: "bidirectional", Direction: goclient.Up, TotalBytes: 42},
	}
	close(events)

	m := newModel(goclient.DefaultConfig())
	m.mode = modeRun
	m.events = events
	m.complete = true

	got, cmd := modelAndCmd(m.Update(eventsMsg{events: []goclient.Event{{
		Kind:   goclient.EventResult,
		Stage:  "bidirectional",
		Result: &goclient.Result{Stage: "bidirectional", Direction: goclient.Down, TotalBytes: 24},
	}}}))
	if cmd == nil {
		t.Fatal("completed runs must keep waiting for buffered events")
	}

	msg := cmd()
	if msg == nil {
		t.Fatal("waitEvent returned nil before draining the buffered upload result")
	}
	got, _ = modelAndCmd(got.Update(msg))
	mm := got

	var sawDown, sawUp bool
	for _, res := range mm.results {
		if res.Stage != "bidirectional" {
			continue
		}
		switch res.Direction {
		case goclient.Down:
			sawDown = true
		case goclient.Up:
			sawUp = true
		}
	}
	if !sawDown || !sawUp {
		t.Fatalf("want both bidirectional transfer results after completion, got %+v", mm.results)
	}
}

func TestThroughputRateAndScale(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.apply(goclient.Event{Kind: goclient.EventThroughput, Direction: goclient.Down, Throughput: goclient.ThroughputSample{BytesPerSec: 100}})
	m.apply(goclient.Event{Kind: goclient.EventThroughput, Direction: goclient.Down, Throughput: goclient.ThroughputSample{BytesPerSec: 200}})
	if got := m.rates[goclient.Down].BytesPerSec; got != 200 {
		t.Errorf("rate = %v, want latest authoritative sample", got)
	}
	// rateScale is the larger peak across both directions.
	m.apply(goclient.Event{Kind: goclient.EventThroughput, Direction: goclient.Up, Throughput: goclient.ThroughputSample{BytesPerSec: 50}})
	if got := m.rateScale(); got != m.peaks[goclient.Down] {
		t.Errorf("rateScale = %v, want the larger peak %v", got, m.peaks[goclient.Down])
	}
}

func TestEventsMsgAppliesBatch(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	got, _ := modelAndCmd(m.Update(eventsMsg{events: []goclient.Event{
		{Kind: goclient.EventStage, Stage: "download"},
		{Kind: goclient.EventThroughput, Direction: goclient.Down, Throughput: goclient.ThroughputSample{BytesPerSec: 1000}},
	}}))
	mm := got
	if mm.stage != "download" || mm.rates[goclient.Down].BytesPerSec != 1000 {
		t.Fatalf("batch was not applied atomically: stage=%q rates=%+v", mm.stage, mm.rates)
	}
}

func TestResultsViewSharedScaleBars(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.results = []goclient.Result{
		{Stage: "download", Direction: goclient.Down, MeanBps: 1000, PeakBps: 1000, TotalBytes: 10},
		{Stage: "upload", Direction: goclient.Up, MeanBps: 500, PeakBps: 500, TotalBytes: 5, ServerAuth: true},
	}
	out := m.resultsView(120)
	if !strings.Contains(out, "download") || !strings.Contains(out, "upload") {
		t.Fatalf("results view missing stage rows:\n%s", out)
	}
	// Upload is half of download and shares the scale, so its bar must have fewer filled cells than download's.
	down := strings.Count(firstLineContaining(out, "download"), "█")
	up := strings.Count(firstLineContaining(out, "upload"), "█")
	if !(down > up && up > 0) {
		t.Errorf("expected upload bar shorter than download on a shared scale: down=%d up=%d", down, up)
	}
}

func firstLineContaining(s, sub string) string {
	for line := range strings.SplitSeq(s, "\n") {
		if strings.Contains(line, sub) {
			return line
		}
	}
	return ""
}

func TestUpdate_WindowSize(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	got, _ := modelAndCmd(m.Update(tea.WindowSizeMsg{Width: 100, Height: 40}))
	mm := got
	if mm.width != 100 {
		t.Errorf("width=%d, want 100", mm.width)
	}
}

func TestUpdate_WindowSizeDuringRun(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.mode = modeRun
	m.stage = "download"
	m.status = "measure"
	m.results = []goclient.Result{{Stage: "latency"}}
	m.events = make(chan goclient.Event)

	got, cmd := modelAndCmd(m.Update(tea.WindowSizeMsg{Width: 120, Height: 45}))
	mm := got
	if mm.width != 120 {
		t.Errorf("width=%d, want 120", mm.width)
	}
	if mm.mode != modeRun || mm.stage != "download" || mm.status != "measure" {
		t.Errorf("resize disturbed run state: mode=%v stage=%q status=%q", mm.mode, mm.stage, mm.status)
	}
	if len(mm.results) != 1 {
		t.Errorf("resize disturbed results, got %+v", mm.results)
	}
	if cmd != nil {
		t.Error("WindowSizeMsg should not produce a follow-up cmd")
	}
}

func TestUpdate_DoneMsg(t *testing.T) {
	boom := errors.New("boom")
	cases := []struct {
		name       string
		err        error
		wantStatus string
		wantErr    error
	}{
		{"no error", nil, "", nil},
		{"error", boom, "error", boom},
		{"context canceled", errors.New("context canceled"), "canceled", nil},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			m, _ := modelAndCmd(newModel(goclient.DefaultConfig()).Update(doneMsg{err: c.err}))
			if !m.complete || m.status != c.wantStatus || m.err != c.wantErr {
				t.Errorf("complete=%v status=%q err=%v, want complete/error=%v/%v and status %q", m.complete, m.status, m.err, true, c.wantErr, c.wantStatus)
			}
		})
	}
}

func TestUpdate_EventsMsg(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.mode = modeRun
	m.events = make(chan goclient.Event)

	got, cmd := modelAndCmd(m.Update(eventsMsg{events: []goclient.Event{
		{Kind: goclient.EventStage, Stage: "x"},
		{Kind: goclient.EventThroughput, Direction: goclient.Down, Throughput: goclient.ThroughputSample{BytesPerSec: 42}},
	}}))
	mm := got
	if mm.stage != "x" {
		t.Errorf("stage = %q, want x", mm.stage)
	}
	if mm.rates[goclient.Down].BytesPerSec != 42 {
		t.Errorf("rate = %v, want 42", mm.rates[goclient.Down].BytesPerSec)
	}
	if cmd == nil {
		t.Error("expected a non-nil cmd to keep waiting for more events")
	}

	mm, cmd = modelAndCmd(mm.Update(eventsMsg{events: []goclient.Event{{Kind: goclient.EventComplete}}}))
	if !mm.complete {
		t.Error("expected complete after EventComplete")
	}
	if cmd == nil {
		t.Error("expected a non-nil cmd to drain buffered events after completion")
	}
}

func TestWaitEventsDrainsBuffered(t *testing.T) {
	events := make(chan goclient.Event, 3)
	for i := range cap(events) {
		events <- goclient.Event{Kind: goclient.EventStage, Stage: string(rune('0' + i))}
	}
	close(events)
	msg, ok := waitEvents(0, events)().(eventsMsg)
	if !ok || len(msg.events) != cap(events) {
		t.Fatalf("waitEvents returned %T with %d events", msg, len(msg.events))
	}
}

func BenchmarkUpdateEventBatch(b *testing.B) {
	batch := make([]goclient.Event, 64)
	for i := range batch {
		direction := goclient.Down
		if i%2 == 1 {
			direction = goclient.Up
		}
		batch[i] = goclient.Event{
			Kind:       goclient.EventThroughput,
			Direction:  direction,
			Throughput: goclient.ThroughputSample{BytesPerSec: float64(100_000_000 + i)},
		}
	}
	events := eventsMsg{events: batch}
	m := newModel(goclient.DefaultConfig())
	m.mode = modeRun
	m.events = make(chan goclient.Event)
	b.ReportAllocs()
	b.ResetTimer()
	for b.Loop() {
		m, _ = modelAndCmd(m.Update(events))
	}
}

// benchmarkView is a package-level sink, so the compiler cannot drop the View call whose cost the benchmarks measure.
var benchmarkView string

func BenchmarkViewConfigure(b *testing.B) {
	m := newModel(goclient.DefaultConfig())
	m.width = 100
	b.ReportAllocs()
	b.ResetTimer()
	for b.Loop() {
		benchmarkView = m.View()
	}
}

func BenchmarkViewTransfer(b *testing.B) {
	m := newModel(goclient.DefaultConfig())
	m.mode = modeRun
	m.width = 100
	m.stage = "bidirectional"
	m.status = "measure"
	m.rates[goclient.Down] = goclient.ThroughputSample{BytesPerSec: 125_000_000, TotalBytes: 1 << 30}
	m.rates[goclient.Up] = goclient.ThroughputSample{BytesPerSec: 75_000_000, TotalBytes: 1 << 30}
	m.peaks[goclient.Down] = 140_000_000
	m.peaks[goclient.Up] = 80_000_000
	m.latency = goclient.LatencySample{RTT: 3 * time.Millisecond}
	m.stages = plannedStages(m.cfg)
	m.stages[0].state = stageDone
	m.stages[1].state, m.stages[1].since = stageMeasuring, m.now.Add(-2*time.Second)
	b.ReportAllocs()
	b.ResetTimer()
	for b.Loop() {
		benchmarkView = m.View()
	}
}

func BenchmarkViewComplete(b *testing.B) {
	m := newModel(goclient.DefaultConfig())
	m.mode = modeRun
	m.width = 100
	m.complete = true
	m.status = "complete"
	m.results = []goclient.Result{{
		Stage:      "download",
		Direction:  goclient.Down,
		MeanBps:    125_000_000,
		PeakBps:    140_000_000,
		TotalBytes: 1 << 30,
	}}
	m.stages = plannedStages(m.cfg)
	b.ReportAllocs()
	b.ResetTimer()
	for b.Loop() {
		benchmarkView = m.View()
	}
}

func TestStaleAuthMessagesDoNotClobberNewerPreparation(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.prepareSeq = 2
	m.prepareStatus = "ready"

	cases := []struct {
		name string
		msg  tea.Msg
	}{
		{"challenge error", authChallengeMsg{seq: 1, err: errors.New("stale challenge")}},
		{"poll error", authTokenMsg{seq: 1, err: errors.New("stale poll")}},
		{"grant", authTokenMsg{seq: 1, token: "grant", origin: "https://elsewhere.example"}},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			got, _ := modelAndCmd(m.Update(c.msg))
			if got.prepareStatus != "ready" || got.prepareError != "" || got.cfg.AuthToken != "" {
				t.Errorf("stale auth changed state: status=%q error=%q token=%q", got.prepareStatus, got.prepareError, got.cfg.AuthToken)
			}
		})
	}
}

func TestCurrentAuthMessagesStillPublishFailure(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.prepareSeq = 2
	m.prepareStatus = "authorizing"

	m, _ = modelAndCmd(m.Update(authTokenMsg{seq: 2, err: errors.New("browser approval timed out")}))
	if m.prepareStatus != "failed" || !strings.Contains(m.prepareError, "browser approval timed out") {
		t.Fatalf("current poll failure = %q %q", m.prepareStatus, m.prepareError)
	}
}

func TestAuthChallengeWaitsForTheOperator(t *testing.T) {
	opened := 0
	m := newModel(goclient.DefaultConfig())
	m.openApproval = func(*goclient.PendingAuthorization) { opened++ }
	m.section, m.row = sectionTiming, 3

	pending := &goclient.PendingAuthorization{BrowserURL: "https://meter.example/auth/cli", Code: "ABCDE"}
	m, _ = modelAndCmd(m.Update(authChallengeMsg{seq: m.prepareSeq, pending: pending}))

	if opened != 0 {
		t.Fatalf("browser opened %d times before a keypress, want 0", opened)
	}
	if m.section != sectionServers || m.row != activePreset(m.cfg.BaseURL) {
		t.Fatalf("approval landed on section %d row %d, want the server row", m.section, m.row)
	}
	if view := ansiPattern.ReplaceAllString(strings.Join(m.authView(), "\n"), ""); !strings.Contains(view, "ABCDE") {
		t.Errorf("approval panel = %q, want the code on show", view)
	}

	m, _ = modelAndCmd(m.Update(tea.KeyMsg{Type: tea.KeyEnter}))
	if opened != 1 || !m.authOpened {
		t.Fatalf("enter opened the browser %d times (authOpened=%v), want once", opened, m.authOpened)
	}

	// With the page open, enter belongs to the selected row again.
	m, _ = modelAndCmd(m.Update(tea.KeyMsg{Type: tea.KeyEnter}))
	if opened != 1 {
		t.Errorf("browser opened %d times, want no second launch", opened)
	}
}

func TestExpiredGrantReturnsToTheServerSelection(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.mode = modeRun
	m.section, m.row = sectionConnections, 2

	m, _ = modelAndCmd(m.Update(doneMsg{seq: m.runSeq, err: &goclient.AuthRequiredError{URL: "https://meter.example/login"}}))
	if m.mode != modeConfigure || m.section != sectionServers {
		t.Fatalf("expired grant left the screen at mode %d section %d", m.mode, m.section)
	}
	if m.prepareStatus != "authorizing" {
		t.Errorf("prepareStatus = %q, want authorizing", m.prepareStatus)
	}
}

// --- layout ---

var (
	ansiPattern     = regexp.MustCompile("\x1b\\[[0-9;]*m")
	selectionMarker = regexp.MustCompile(`│ +› `)
)

func TestView_DrawsOneSelectionPerScreen(t *testing.T) {
	for _, width := range []int{80, 120, 200} {
		m := newModel(goclient.DefaultConfig())
		m.width = width
		for sec := range sectionCount {
			m.section = sec
			for row := range m.rowCount() {
				m.row = row
				view := ansiPattern.ReplaceAllString(m.View(), "")
				// The marker opens a panel body line; "‹1/2›" cycle positions sit further along the line.
				if got := len(selectionMarker.FindAllString(view, -1)); got != 1 {
					t.Errorf("width %d section %d row %d: %d selection markers, want 1", width, sec, row, got)
				}
				for _, label := range sectionLabels {
					if !strings.Contains(view, label) {
						t.Errorf("width %d: tab bar is missing %q", width, label)
					}
				}
			}
		}
	}
}

func TestFinalReport(t *testing.T) {
	profile := lipgloss.ColorProfile()
	defer lipgloss.SetColorProfile(profile)
	lipgloss.SetColorProfile(termenv.TrueColor)

	m := newModel(goclient.DefaultConfig())
	m.width = 120
	m.results = []goclient.Result{{Stage: "download", Direction: goclient.Down, MeanBps: 1e6, PeakBps: 2e6, TotalBytes: 5e6}}
	if report := m.finalReport(); report != "" {
		t.Errorf("report before the run completed = %q, want none", report)
	}

	m.complete = true
	report := m.finalReport()
	if !strings.Contains(report, "download") || !strings.Contains(report, "Mbit/s") {
		t.Errorf("report = %q, want the download result", report)
	}
	if strings.ContainsRune(report, '\x1b') {
		t.Error("report carries terminal styling into the scrollback")
	}
}

func TestConnectionChecksFollowTheHandshake(t *testing.T) {
	cases := []struct {
		name  string
		setup func(*model)
		want  []checkState
	}{
		{
			name:  "not checked",
			setup: func(m *model) {},
			want:  []checkState{checkPending, checkPending, checkPending, checkPending},
		},
		{
			name: "checking",
			setup: func(m *model) {
				m.prepareStatus, m.prepareStep = "checking", stepReach
			},
			want: []checkState{checkActive, checkPending, checkPending, checkPending},
		},
		{
			name: "unreachable",
			setup: func(m *model) {
				m.prepareStatus, m.prepareStep = "failed", stepReach
			},
			want: []checkState{checkFailed, checkPending, checkPending, checkPending},
		},
		{
			name: "authorization demanded by the preflight itself",
			setup: func(m *model) {
				m.prepareStatus, m.prepareStep = "authorizing", stepPreflight
			},
			want: []checkState{checkDone, checkPending, checkActive, checkPending},
		},
		{
			name: "authorization demanded by a target probe",
			setup: func(m *model) {
				m.prepareStatus, m.prepareStep = "authorizing", stepOrigins
			},
			want: []checkState{checkDone, checkDone, checkActive, checkPending},
		},
		{
			name: "target selection failed",
			setup: func(m *model) {
				m.prepareStatus, m.prepareStep = "failed", stepOrigins
			},
			want: []checkState{checkDone, checkDone, checkPending, checkFailed},
		},
		{
			name: "ready on a server that asks for nothing",
			setup: func(m *model) {
				m.prepareStatus, m.prepareStep = "ready", stepReady
			},
			want: []checkState{checkDone, checkDone, checkSkipped, checkDone},
		},
		{
			name: "ready with a granted token",
			setup: func(m *model) {
				m.prepareStatus, m.prepareStep = "ready", stepReady
				m.cfg.AuthToken = "grant"
			},
			want: []checkState{checkDone, checkDone, checkDone, checkDone},
		},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			m := newModel(goclient.DefaultConfig())
			c.setup(&m)
			checks := m.connectionChecks()
			if len(checks) != len(c.want) {
				t.Fatalf("checks = %d, want %d", len(checks), len(c.want))
			}
			for i, want := range c.want {
				if checks[i].state != want {
					t.Errorf("%q state = %v, want %v", checks[i].label, checks[i].state, want)
				}
			}
		})
	}
}

func TestPreparationMessageRecordsHowFarItGot(t *testing.T) {
	pf := wire.Preflight{Server: wire.ServerInfo{Name: "srv"}}
	cases := []struct {
		name       string
		msg        preparationMsg
		wantStep   prepareStep
		wantStatus string
	}{
		{
			name:       "transport failure proves nothing",
			msg:        preparationMsg{err: errors.New("connection refused")},
			wantStep:   stepReach,
			wantStatus: "failed",
		},
		{
			name:       "a preflight body proves the server answered",
			msg:        preparationMsg{err: &goclient.PreparationError{Preflight: pf, Err: errors.New("no usable endpoint")}},
			wantStep:   stepOrigins,
			wantStatus: "failed",
		},
		{
			name:       "a challenge at the preflight proves reachability only",
			msg:        preparationMsg{err: &goclient.AuthRequiredError{URL: "https://meter.example/login"}},
			wantStep:   stepPreflight,
			wantStatus: "authorizing",
		},
		{
			name: "a challenge at a target probe proves the preflight too",
			msg: preparationMsg{err: &goclient.PreparationError{
				Preflight: pf,
				Err:       &goclient.AuthRequiredError{URL: "https://meter.example/login"},
			}},
			wantStep:   stepOrigins,
			wantStatus: "authorizing",
		},
		{
			name:       "success proves the whole handshake",
			msg:        preparationMsg{connection: &goclient.PreparedConnection{Preflight: pf}},
			wantStep:   stepReady,
			wantStatus: "ready",
		},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			m := newModel(goclient.DefaultConfig())
			c.msg.seq = m.prepareSeq
			m, _ = modelAndCmd(m.Update(c.msg))
			if m.prepareStep != c.wantStep || m.prepareStatus != c.wantStatus {
				t.Fatalf("step/status = %v/%q, want %v/%q", m.prepareStep, m.prepareStatus, c.wantStep, c.wantStatus)
			}
		})
	}
}

func TestStageTimelineFollowsStageEvents(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.stages = plannedStages(m.cfg)
	if len(m.stages) != 3 {
		t.Fatalf("planned stages = %d, want latency, download and upload", len(m.stages))
	}

	start := m.now
	m.apply(goclient.Event{Kind: goclient.EventStage, At: start, Stage: "latency", Message: "measure"})
	if m.stages[0].state != stageMeasuring || !m.stages[0].since.Equal(start) {
		t.Fatalf("latency = %v since %v, want measuring since %v", m.stages[0].state, m.stages[0].since, start)
	}
	if m.stages[1].state != stagePending {
		t.Errorf("download = %v, want pending", m.stages[1].state)
	}

	m.apply(goclient.Event{Kind: goclient.EventResult, Stage: "latency", Result: &goclient.Result{Stage: "latency"}})
	if m.stages[0].state != stageDone {
		t.Errorf("latency after its result = %v, want done", m.stages[0].state)
	}

	warmupAt := start.Add(4 * time.Second)
	m.apply(goclient.Event{Kind: goclient.EventStage, At: warmupAt, Stage: "download", Message: "warmup"})
	if m.stages[1].state != stageWarmup {
		t.Fatalf("download = %v, want warmup", m.stages[1].state)
	}
	m.apply(goclient.Event{Kind: goclient.EventStage, At: warmupAt.Add(time.Second), Stage: "download", Message: "measure"})
	if m.stages[1].state != stageMeasuring {
		t.Fatalf("download = %v, want measuring", m.stages[1].state)
	}

	m.apply(goclient.Event{Kind: goclient.EventResult, Stage: "download", Result: &goclient.Result{Stage: "download", Direction: goclient.Down}})
	m.apply(goclient.Event{Kind: goclient.EventResult, Stage: "download", Result: &goclient.Result{Stage: "download"}})
	if m.stages[1].state != stageDone {
		t.Errorf("download after its results = %v, want done", m.stages[1].state)
	}

	m.apply(goclient.Event{Kind: goclient.EventStage, At: warmupAt.Add(10 * time.Second), Stage: "upload", Message: "measure"})
	m.apply(goclient.Event{Kind: goclient.EventError, Err: errors.New("upload failed")})
	if m.stages[2].state != stageStopped {
		t.Errorf("upload after the run failed = %v, want stopped", m.stages[2].state)
	}
}

func TestPlannedStages(t *testing.T) {
	cfg := goclient.DefaultConfig()
	cfg.LatencyDuration = time.Second
	cfg.DownloadDuration = 2 * time.Second
	cfg.UploadDuration = 3 * time.Second
	cfg.BidirectionalDuration = 4 * time.Second
	cases := []struct {
		name  string
		stage goclient.StageSet
		want  []stageProgress
	}{
		{"none", goclient.StageSet{}, nil},
		{
			name:  "engine order, not selection order",
			stage: goclient.StageSet{Bidirectional: true, Upload: true, Download: true, Latency: true},
			want: []stageProgress{
				{name: "latency", duration: time.Second},
				{name: "download", duration: 2 * time.Second},
				{name: "upload", duration: 3 * time.Second},
				{name: "bidirectional", duration: 4 * time.Second},
			},
		},
		{
			name:  "disabled stages leave no row",
			stage: goclient.StageSet{Latency: true, Bidirectional: true},
			want: []stageProgress{
				{name: "latency", duration: time.Second},
				{name: "bidirectional", duration: 4 * time.Second},
			},
		},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			cfg.Stages = c.stage
			got := plannedStages(cfg)
			if len(got) != len(c.want) {
				t.Fatalf("stages = %+v, want %+v", got, c.want)
			}
			for i := range c.want {
				if got[i] != c.want[i] {
					t.Errorf("stage %d = %+v, want %+v", i, got[i], c.want[i])
				}
			}
		})
	}
}

func TestStageTimelineIgnoresWhatItCannotPlace(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.stages = plannedStages(m.cfg)

	m.apply(goclient.Event{Kind: goclient.EventResult, Stage: "download", Result: &goclient.Result{Stage: "download"}})
	if m.stages[1].state != stagePending {
		t.Errorf("download = %v after a result but no start, want pending", m.stages[1].state)
	}

	m.apply(goclient.Event{Kind: goclient.EventStage, Stage: "download", Message: "measure"})
	m.apply(goclient.Event{Kind: goclient.EventStage, Stage: "download", Message: "cooldown"})
	if m.stages[1].state != stageMeasuring {
		t.Errorf("download = %v after an unnamed phase, want measuring", m.stages[1].state)
	}

	m.apply(goclient.Event{Kind: goclient.EventStage, Stage: "loaded-latency", Message: "measure"})
	if m.stages[0].state != stagePending || m.stages[2].state != stagePending {
		t.Errorf("a stage with no row disturbed the timeline: %+v", m.stages)
	}
}

func TestEndOfRunStopsTheStageThatWasRunning(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.mode = modeRun
	m.stages = plannedStages(m.cfg)
	m.stages[0].state = stageDone
	m.stages[1].state = stageMeasuring

	m, _ = modelAndCmd(m.Update(doneMsg{err: errors.New("context canceled")}))
	if m.stages[0].state != stageDone || m.stages[1].state != stageStopped || m.stages[2].state != stagePending {
		t.Errorf("timeline after a canceled run = %+v, want the running stage stopped only", m.stages)
	}
}

func TestTimelineShowsElapsedAgainstTheConfiguredWindow(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.width = 120
	m.mode = modeRun
	m.stages = plannedStages(m.cfg)
	m.stages[1].state, m.stages[1].since = stageMeasuring, m.now.Add(-6200*time.Millisecond)

	line := ansiPattern.ReplaceAllString(strings.Join(m.timelineView(60), "\n"), "")
	if !strings.Contains(line, "6.2s / 10s") {
		t.Errorf("measuring row = %q, want the elapsed and configured window", line)
	}
	if !strings.Contains(line, "█") {
		t.Errorf("measuring row = %q, want a determinate bar", line)
	}
	if !strings.Contains(line, "upload") || !strings.Contains(line, "pending") {
		t.Errorf("timeline = %q, want the stages that have not started yet", line)
	}
}

func TestTimelineWarmupCountsUpWithoutATotal(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.stages = plannedStages(m.cfg)
	m.stages[1].state, m.stages[1].since = stageWarmup, m.now.Add(-1400*time.Millisecond)

	lines := ansiPattern.ReplaceAllString(strings.Join(m.timelineView(60), "\n"), "")
	if !strings.Contains(lines, "warmup") || !strings.Contains(lines, "1.4s") {
		t.Errorf("warmup rows = %q, want a labelled elapsed clock", lines)
	}
	if strings.Contains(lines, "/ 800ms") || strings.Contains(lines, "█") {
		t.Errorf("warmup rows = %q, want no window the client cannot know", lines)
	}
}

func TestAuthWaitShowsTheCodeAndTheDeadline(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.prepareStatus, m.prepareStep = "authorizing", stepOrigins
	m.auth = &goclient.PendingAuthorization{BrowserURL: "https://meter.example/auth/cli?challenge=x", Code: "AB2C4DZ"}
	m.authSince = m.now.Add(-13 * time.Second)

	view := ansiPattern.ReplaceAllString(strings.Join(m.authView(), "\n"), "")
	for _, want := range []string{"https://meter.example/auth/cli?challenge=x", "AB2C4DZ", "waiting 13.0s", "expires in 1m47s"} {
		if !strings.Contains(view, want) {
			t.Errorf("auth wait = %q, want %q", view, want)
		}
	}

	m.auth = nil
	if lines := m.authView(); lines != nil {
		t.Errorf("auth wait without a pending approval = %v, want nothing", lines)
	}
}

func TestLatencyLineHoldsTheLastRoundTripThroughLosses(t *testing.T) {
	cases := []struct {
		name   string
		sample goclient.LatencySample
		streak int
		want   string
	}{
		{"no sample yet", goclient.LatencySample{}, 0, "waiting"},
		{"only losses so far", goclient.LatencySample{}, 2, "timeout"},
		{"round trip", goclient.LatencySample{RTT: 3 * time.Millisecond}, 0, "3.00 ms"},
		{"short streak keeps the value", goclient.LatencySample{RTT: 3 * time.Millisecond}, 1, "3.00 ms  1 lost"},
		{"sustained streak reads as timeout", goclient.LatencySample{RTT: 3 * time.Millisecond}, 4, "timeout ×4"},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			got := ansiPattern.ReplaceAllString(latencyLine(c.sample, c.streak), "")
			if !strings.Contains(got, c.want) {
				t.Errorf("latencyLine(%+v, %d) = %q, want %q", c.sample, c.streak, got, c.want)
			}
		})
	}
}

func TestApplyLatencyKeepsTheLastRoundTripThroughLosses(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.apply(goclient.Event{Kind: goclient.EventLatency, Latency: goclient.LatencySample{RTT: 5 * time.Millisecond}})
	m.apply(goclient.Event{Kind: goclient.EventLatency, Latency: goclient.LatencySample{Lost: true}})
	m.apply(goclient.Event{Kind: goclient.EventLatency, Latency: goclient.LatencySample{Lost: true}})
	if m.latency.RTT != 5*time.Millisecond || m.lostStreak != 2 {
		t.Errorf("after two losses: rtt=%v streak=%d, want the held 5ms and streak 2", m.latency.RTT, m.lostStreak)
	}
	m.apply(goclient.Event{Kind: goclient.EventLatency, Latency: goclient.LatencySample{RTT: 7 * time.Millisecond}})
	if m.latency.RTT != 7*time.Millisecond || m.lostStreak != 0 {
		t.Errorf("a pong should adopt the new value and clear the streak: rtt=%v streak=%d", m.latency.RTT, m.lostStreak)
	}
}

func TestFmtClock(t *testing.T) {
	cases := []struct {
		d    time.Duration
		want string
	}{
		{-time.Second, "0.0s"},
		{1400 * time.Millisecond, "1.4s"},
		{59500 * time.Millisecond, "59.5s"},
		{107 * time.Second, "1m47s"},
	}
	for _, c := range cases {
		if got := fmtClock(c.d); got != c.want {
			t.Errorf("fmtClock(%v) = %q, want %q", c.d, got, c.want)
		}
	}
}

func TestViewNeverExceedsTheTerminalWidth(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.cfg.BaseURL = "https://a-very-long-hostname.internal.example.com:7247"
	m.notice = strings.Repeat("a long notice ", 12)
	for _, width := range []int{44, 60, 80, 120, 200} {
		m.width = width
		for _, sec := range []section{sectionServers, sectionConnections, sectionRun} {
			m.section = sec
			for i, line := range strings.Split(m.View(), "\n") {
				if got := lipgloss.Width(line); got > width {
					t.Errorf("width %d, section %v, line %d spans %d cells", width, sec, i, got)
				}
			}
		}
	}

	m.mode = modeRun
	m.server = "graphite-meter somewhere [https://a-very-long-hostname.internal.example.com:7248/http3]"
	m.stages = plannedStages(m.cfg)
	m.results = []goclient.Result{{Stage: "download", Direction: goclient.Down, MeanBps: 1e9, PeakBps: 2e9, TotalBytes: 1e10}}
	for _, width := range []int{44, 80, 200} {
		m.width = width
		for i, line := range strings.Split(m.View(), "\n") {
			if got := lipgloss.Width(line); got > width {
				t.Errorf("run view, width %d, line %d spans %d cells", width, i, got)
			}
		}
	}
}

func TestRenderBarMovesInSubCellSteps(t *testing.T) {
	plain := func(v float64) string { return ansiPattern.ReplaceAllString(renderBar(v, 100, 10, false), "") }
	if got := plain(0); got != strings.Repeat("░", 10) {
		t.Errorf("empty bar = %q", got)
	}
	if got := plain(100); got != strings.Repeat("█", 10) {
		t.Errorf("full bar = %q", got)
	}
	half, quarterStep := plain(50), plain(52.5)
	if half == quarterStep {
		t.Errorf("a quarter-cell advance did not move the tip: %q", half)
	}
	if !strings.Contains(quarterStep, "▎") {
		t.Errorf("bar at 5.25 cells = %q, want a two-eighths tip", quarterStep)
	}
	if w := lipgloss.Width(renderBar(52.5, 100, 10, true)); w != 10 {
		t.Errorf("bar width with a partial tip = %d, want 10", w)
	}
}

func TestConnectionPathRowSurvivesTheTwoColumnThreshold(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.cfg.BaseURL = "https://meter.example:7247"
	m.section, m.row = sectionConnections, rowThroughputPath
	m.discovery = &wire.Preflight{Capabilities: wire.Capabilities{
		ThroughputTargets: []wire.ThroughputTarget{
			{Origin: "https://meter.example:7247", Transport: wire.TransportFetchStream, Protocol: protocolNegotiated, TLS: true},
			{Origin: "https://meter.example:7249", Transport: wire.TransportWebTransport, Protocol: "http3", TLS: true},
		},
	}}
	m.cfg.ThroughputTarget, m.cfg.ThroughputTransport = "https://meter.example:7249", wire.TransportWebTransport

	for _, width := range []int{twoColumnMin + shellMargin*2, twoColumnMin + shellMargin*2 - 1, 200} {
		m.width = width
		view := ansiPattern.ReplaceAllString(m.View(), "")
		for _, want := range []string{"Throughput path", "WebTransport · HTTP/3 · TLS", "‹3/3›", ":7249"} {
			if !strings.Contains(view, want) {
				t.Errorf("at width %d the path row lost %q:\n%s", width, want, view)
			}
		}
	}
}

func TestPathRowNamesTheChoice(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.cfg.BaseURL = "https://meter.example:7247"
	m.discovery = &wire.Preflight{Capabilities: wire.Capabilities{
		ThroughputTargets: []wire.ThroughputTarget{
			{Origin: "https://meter.example:7247", Transport: wire.TransportFetchStream, Protocol: "http1", TLS: true},
			{Origin: "https://meter.example:7248", Transport: wire.TransportFetchStream, Protocol: "http2", TLS: true},
			{Origin: "https://meter.example:7249", Transport: wire.TransportWebTransport, Protocol: "http3", TLS: true},
			{Origin: "https://elsewhere.example", Transport: wire.TransportFetchStream, Protocol: protocolNegotiated, TLS: true},
		},
	}}
	choices := m.throughputPaths()
	row := func(target, transport string) string {
		return ansiPattern.ReplaceAllString(pathRow("Throughput path", target, transport, choices), "")
	}

	got := row("https://meter.example:7248", wire.TransportFetchStream)
	for _, want := range []string{"Throughput path", "Fetch stream · HTTP/2 · TLS", "‹3/5›", ":7248"} {
		if !strings.Contains(got, want) {
			t.Errorf("path row = %q, want %q", got, want)
		}
	}

	session := row("https://meter.example:7249", wire.TransportWebTransport)
	for _, want := range []string{"WebTransport · HTTP/3 · TLS", "‹4/5›", ":7249"} {
		if !strings.Contains(session, want) {
			t.Errorf("session path row = %q, want %q", session, want)
		}
	}

	// An origin on another host is named by that host, not by a bare port.
	other := row("https://elsewhere.example", wire.TransportFetchStream)
	for _, want := range []string{"Fetch stream · Negotiated · TLS", "elsewhere.example"} {
		if !strings.Contains(other, want) {
			t.Errorf("off-host path row = %q, want %q", other, want)
		}
	}

	automatic := row("auto", "auto")
	for _, want := range []string{"Automatic", "‹1/5›"} {
		if !strings.Contains(automatic, want) {
			t.Errorf("automatic path row = %q, want %q", automatic, want)
		}
	}
}

func populatedRunModel(width int) model {
	m := newModel(goclient.DefaultConfig())
	m.mode = modeRun
	m.width = width
	m.stage = "bidirectional"
	m.status = "measure"
	m.rates[goclient.Down] = goclient.ThroughputSample{BytesPerSec: 125_000_000, TotalBytes: 1 << 30}
	m.rates[goclient.Up] = goclient.ThroughputSample{BytesPerSec: 75_000_000, TotalBytes: 1 << 30}
	m.peaks[goclient.Down] = 940_000_000
	m.peaks[goclient.Up] = 80_000_000
	m.latency = goclient.LatencySample{RTT: 3 * time.Millisecond}
	m.target = "throughput.example.com — a deliberately long target label to test truncation at narrow widths"
	m.stages = plannedStages(m.cfg)
	m.stages[0].state = stageDone
	m.stages[1].state, m.stages[1].since = stageMeasuring, m.now.Add(-2*time.Second)
	return m
}

func frameBound(width int) int { return max(width, 44) }

func TestRenderedFramesNeverExceedWidth(t *testing.T) {
	for _, width := range []int{40, 60, 80, 100, 140, 200} {
		bound := frameBound(width)
		frames := map[string]string{
			"configure": func() string { m := newModel(goclient.DefaultConfig()); m.width = width; return m.View() }(),
			"run":       populatedRunModel(width).View(),
		}
		for name, frame := range frames {
			for i, line := range strings.Split(frame, "\n") {
				if w := lipgloss.Width(line); w > bound {
					t.Errorf("%s@%d: line %d is %d cells wide (> %d): %q", name, width, i, w, bound, line)
				}
			}
		}
	}
}

func TestRenderRunFrame(t *testing.T) {
	frame := populatedRunModel(100).View()
	t.Log("\n" + frame)
	for _, want := range []string{"Session", "Live Telemetry", "bidirectional / measure", "3.00 ms", "Stages"} {
		if !strings.Contains(frame, want) {
			t.Errorf("run frame missing %q", want)
		}
	}
}

func TestRunViewNamesTheCommittedPathsRatherThanTheProbes(t *testing.T) {
	m := populatedRunModel(160)
	m.target, m.latencyTarget = "https://meter.example:7249", "https://meter.example:7249"
	m.throughputTransport, m.latencyTransport = wire.TransportWebTransport, wire.TransportWebTransport
	m.throughputProtocol, m.latencyProtocol = "h3", "h3"

	view := ansiPattern.ReplaceAllString(m.View(), "")
	if strings.Contains(view, "HTTP/1.1") {
		t.Errorf("a WebTransport run reports HTTP/1.1:\n%s", view)
	}
	if strings.Count(view, "WebTransport · HTTP/3 · TLS") != 2 {
		t.Errorf("run view does not name both committed paths:\n%s", view)
	}
	if !strings.Contains(view, "meter.example:7249") {
		t.Errorf("run view drops the origin carrying the paths:\n%s", view)
	}

	blank := newModel(goclient.DefaultConfig())
	blank.mode, blank.width = modeRun, 160
	if strings.Contains(ansiPattern.ReplaceAllString(blank.View(), ""), " ·  · ") {
		t.Error("an unannounced run renders an empty path summary")
	}
}

func TestRunViewNamesThePerDirectionLaneCount(t *testing.T) {
	m := populatedRunModel(140)
	m.throughputProtocol = "h2"
	m.throughputTransport = wire.TransportFetchStream
	view := ansiPattern.ReplaceAllString(m.View(), "")
	if !strings.Contains(view, "Automatic · 1 download / 4 upload") {
		t.Errorf("run view does not name the per-direction lane count:\n%s", view)
	}
}
