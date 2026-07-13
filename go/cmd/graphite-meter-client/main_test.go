package main

import (
	"errors"
	"strings"
	"testing"
	"time"

	tea "github.com/charmbracelet/bubbletea"
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
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			if got := parsePing(c.raw); got != c.want {
				t.Errorf("parsePing(%q) = %v, want %v", c.raw, got, c.want)
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

func TestFmtRate(t *testing.T) {
	cases := []struct {
		bps  float64
		want string
	}{
		{0, "0 bit/s"},
		{12.5, "100 bit/s"},
		{125, "1.00 Kbit/s"},
		{125000, "1.00 Mbit/s"},
		{125000000, "1.00 Gbit/s"},
		{125000000000, "1.00 Tbit/s"},
		{125000000000000, "1000.00 Tbit/s"}, // caps at the last unit rather than inventing one
	}
	for _, c := range cases {
		if got := fmtRate(c.bps); got != c.want {
			t.Errorf("fmtRate(%v) = %q, want %q", c.bps, got, c.want)
		}
	}
}

func TestFmtBytes(t *testing.T) {
	cases := []struct {
		n    uint64
		want string
	}{
		{0, "0 B"},
		{500, "500 B"},
		{1000, "1.00 KB"},
		{1500000, "1.50 MB"},
		{1250000000, "1.25 GB"},
		{1000000000000000, "1000.00 TB"}, // caps at the last unit rather than inventing one
	}
	for _, c := range cases {
		if got := fmtBytes(c.n); got != c.want {
			t.Errorf("fmtBytes(%d) = %q, want %q", c.n, got, c.want)
		}
	}
}

func TestFmtMs(t *testing.T) {
	cases := []struct {
		d    time.Duration
		want string
	}{
		{0, "--"},
		{-time.Millisecond, "--"},
		{1500 * time.Microsecond, "1.50 ms"},
		{2 * time.Second, "2000.00 ms"},
		{3*time.Millisecond + 250*time.Microsecond, "3.25 ms"},
	}
	for _, c := range cases {
		if got := fmtMs(c.d); got != c.want {
			t.Errorf("fmtMs(%v) = %q, want %q", c.d, got, c.want)
		}
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
	want := []string{"Latency baseline for 2s", "Download for 5s"}
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
	if got := activePreset(serverPresets[0].url); got != 0 {
		t.Errorf("activePreset(preset0) = %d, want 0", got)
	}
	if got := activePreset(serverPresets[1].url); got != 1 {
		t.Errorf("activePreset(preset1) = %d, want 1", got)
	}
	if got := activePreset("http://example.invalid:9999"); got != -1 {
		t.Errorf("activePreset(unknown) = %d, want -1", got)
	}
}

func TestTimingLabel(t *testing.T) {
	want := []string{"Warmup", "Latency duration", "Download duration", "Upload duration", "Bidirectional duration", "Ping interval"}
	for i, w := range want {
		if got := timingLabel(i); got != w {
			t.Errorf("timingLabel(%d) = %q, want %q", i, got, w)
		}
	}
	if got := timingLabel(99); got != "" {
		t.Errorf("timingLabel(out of range) = %q, want empty", got)
	}
}

func TestBoolLabel(t *testing.T) {
	if got := boolLabel(true); got != "enabled" {
		t.Errorf("boolLabel(true) = %q, want enabled", got)
	}
	if got := boolLabel(false); got != "disabled" {
		t.Errorf("boolLabel(false) = %q, want disabled", got)
	}
}

func TestTLSLabel(t *testing.T) {
	if got := tlsLabel(true); got != "verification skipped" {
		t.Errorf("tlsLabel(true) = %q, want verification skipped", got)
	}
	if got := tlsLabel(false); got != "verified" {
		t.Errorf("tlsLabel(false) = %q, want verified", got)
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
		{sectionStages, 5},
		{sectionTiming, 6},
		{sectionNetwork, 6},
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

func TestHandleKey_TabCyclesSections(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	order := []section{sectionStages, sectionTiming, sectionNetwork, sectionRun, sectionServers}
	for _, want := range order {
		next, _ := m.handleKey(tea.KeyMsg{Type: tea.KeyTab})
		m = next.(model)
		if m.section != want {
			t.Fatalf("section after tab = %v, want %v", m.section, want)
		}
	}
}

func TestHandleKey_ShiftTabCyclesSectionsBackward(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	next, _ := m.handleKey(tea.KeyMsg{Type: tea.KeyShiftTab})
	m = next.(model)
	if m.section != sectionRun {
		t.Errorf("section after shift+tab from sectionServers = %v, want sectionRun", m.section)
	}
}

func TestHandleKey_RightLeftCycleSections(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	next, _ := m.handleKey(tea.KeyMsg{Type: tea.KeyRight})
	m = next.(model)
	if m.section != sectionStages {
		t.Errorf("section after right = %v, want sectionStages", m.section)
	}
	next, _ = m.handleKey(tea.KeyMsg{Type: tea.KeyLeft})
	m = next.(model)
	if m.section != sectionServers {
		t.Errorf("section after left = %v, want sectionServers", m.section)
	}
}

func TestHandleKey_RowNavigationClamped(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.section = sectionStages // rowCount == 5, valid rows 0..4
	m.row = 4

	next, _ := m.handleKey(tea.KeyMsg{Type: tea.KeyDown})
	m = next.(model)
	if m.row != 4 {
		t.Errorf("row after down at max = %d, want clamped to 4", m.row)
	}

	next, _ = m.handleKey(tea.KeyMsg{Type: tea.KeyUp})
	m = next.(model)
	if m.row != 3 {
		t.Errorf("row after up = %d, want 3", m.row)
	}

	next, _ = m.handleKey(keyRunes("k"))
	m = next.(model)
	if m.row != 2 {
		t.Errorf("row after 'k' = %d, want 2", m.row)
	}

	next, _ = m.handleKey(keyRunes("j"))
	m = next.(model)
	if m.row != 3 {
		t.Errorf("row after 'j' = %d, want 3", m.row)
	}

	m.row = 0
	next, _ = m.handleKey(tea.KeyMsg{Type: tea.KeyUp})
	m = next.(model)
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
		{"network", sectionNetwork, 6},
		{"run (single row)", sectionRun, 1},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			m := newModel(goclient.DefaultConfig())
			m.section = c.section

			m.row = 0
			next, _ := m.handleKey(tea.KeyMsg{Type: tea.KeyUp})
			m = next.(model)
			if m.row != 0 {
				t.Errorf("row after up at the first row = %d, want clamped to 0", m.row)
			}

			m.row = c.rows - 1
			next, _ = m.handleKey(tea.KeyMsg{Type: tea.KeyDown})
			m = next.(model)
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

	next, _ := m.activate()
	m = next.(model)
	if m.edit.kind != editURL {
		t.Fatalf("edit.kind after starting an edit = %v, want editURL", m.edit.kind)
	}
	baseline := m.edit.value

	next, _ = m.handleKey(keyRunes("x"))
	m = next.(model)
	next, _ = m.handleKey(tea.KeyMsg{Type: tea.KeyEsc})
	m = next.(model)
	if m.edit.kind != editNone {
		t.Fatalf("edit.kind after cancel = %v, want editNone", m.edit.kind)
	}
	if m.cfg.BaseURL != baseline {
		t.Errorf("BaseURL changed to %q after a cancelled edit, want unchanged %q", m.cfg.BaseURL, baseline)
	}

	// Re-entering the edit should reflect the still-unchanged config, not the
	// cancelled "x" typed in the previous attempt.
	next, _ = m.activate()
	m = next.(model)
	if m.edit.value != baseline {
		t.Errorf("edit.value on re-entry = %q, want the unchanged BaseURL %q (not the cancelled edit)", m.edit.value, baseline)
	}

	next, _ = m.handleKey(keyRunes("y"))
	m = next.(model)
	next, _ = m.handleKey(tea.KeyMsg{Type: tea.KeyEnter})
	m = next.(model)
	if m.edit.kind != editNone {
		t.Errorf("edit.kind after commit = %v, want editNone", m.edit.kind)
	}
	if want := baseline + "y"; m.cfg.BaseURL != want {
		t.Errorf("BaseURL after committing the re-edit = %q, want %q", m.cfg.BaseURL, want)
	}
}

func TestHandleKey_QuitSendsCancelAndQuit(t *testing.T) {
	for _, key := range []tea.KeyMsg{{Type: tea.KeyCtrlC}, keyRunes("q")} {
		called := false
		m := newModel(goclient.DefaultConfig())
		m.cancel = func() { called = true }

		next, cmd := m.handleKey(key)
		if !called {
			t.Errorf("key %q did not invoke cancel", key.String())
		}
		if cmd == nil {
			t.Fatalf("key %q returned nil cmd, want tea.Quit", key.String())
		}
		if _, ok := cmd().(tea.QuitMsg); !ok {
			t.Errorf("key %q cmd() did not produce tea.QuitMsg", key.String())
		}
		_ = next
	}
}

func TestHandleKey_EscInConfigureSetsNotice(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	next, _ := m.handleKey(tea.KeyMsg{Type: tea.KeyEsc})
	m = next.(model)
	if !strings.Contains(m.notice, "Configuration kept") {
		t.Errorf("notice after esc = %q, want mention of Configuration kept", m.notice)
	}
}

func TestActivate_ServerPreset(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.section = sectionServers
	m.row = 1

	next, _ := m.activate()
	m = next.(model)
	if m.cfg.BaseURL != serverPresets[1].url {
		t.Errorf("BaseURL after activating preset 1 = %q, want %q", m.cfg.BaseURL, serverPresets[1].url)
	}
	if !strings.Contains(m.notice, serverPresets[1].name) {
		t.Errorf("notice = %q, want mention of %q", m.notice, serverPresets[1].name)
	}
}

func TestActivate_ServerCustomStartsEdit(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.section = sectionServers
	m.row = len(serverPresets)

	next, _ := m.activate()
	m = next.(model)
	if m.edit.kind != editURL {
		t.Errorf("edit.kind = %v, want editURL", m.edit.kind)
	}
	if m.edit.value != m.cfg.BaseURL {
		t.Errorf("edit.value = %q, want current BaseURL %q", m.edit.value, m.cfg.BaseURL)
	}
}

func TestActivate_StagesToggle(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.section = sectionStages
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
		next, _ := m.activate()
		m = next.(model)
		if get(m.cfg) == before {
			t.Errorf("row %d did not toggle: before=%v after=%v", row, before, get(m.cfg))
		}
		cfg = m.cfg
	}
}

func TestActivate_TimingStartsEdit(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.section = sectionTiming
	fields := []string{"warmup", "latency", "download", "upload", "bidirectional", "ping"}
	for row, field := range fields {
		m.row = row
		next, _ := m.activate()
		mm := next.(model)
		if mm.edit.kind != editDuration {
			t.Errorf("row %d edit.kind = %v, want editDuration", row, mm.edit.kind)
		}
		if mm.edit.field != field {
			t.Errorf("row %d edit.field = %q, want %q", row, mm.edit.field, field)
		}
		if mm.edit.value != m.durationValue(row) {
			t.Errorf("row %d edit.value = %q, want %q", row, mm.edit.value, m.durationValue(row))
		}
	}
}

func TestActivate_NetworkStreamSettingsStartTheirEditors(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.section = sectionNetwork
	for row, field := range map[int]string{2: "auto-streams", 3: "streams"} {
		m.row = row
		next, _ := m.activate()
		edited := next.(model)
		if edited.edit.kind != editInt || edited.edit.field != field {
			t.Errorf("row %d edit = %+v, want editInt %q", row, edited.edit, field)
		}
	}
}

func TestActivate_NetworkTLSToggle(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.section = sectionNetwork
	m.row = 4
	before := m.cfg.InsecureSkipTLSVerify
	next, _ := m.activate()
	m = next.(model)
	if m.cfg.InsecureSkipTLSVerify == before {
		t.Error("InsecureSkipTLSVerify was not toggled")
	}
}

func TestActivate_NetworkReset(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.cfg.TransferStreams.Forced = 99
	m.cfg.BaseURL = "http://changed.example"
	m.section = sectionNetwork
	m.row = 5
	next, _ := m.activate()
	m = next.(model)
	want := goclient.DefaultConfig()
	if m.cfg != want {
		t.Errorf("cfg after reset = %+v, want default %+v", m.cfg, want)
	}
}

func TestHandleEditKey_TypeBackspaceCommitCancel(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.edit = editState{kind: editURL, field: "url", value: ""}

	next, _ := m.handleKey(keyRunes("h"))
	m = next.(model)
	next, _ = m.handleKey(keyRunes("i"))
	m = next.(model)
	if m.edit.value != "hi" {
		t.Fatalf("edit.value after typing = %q, want %q", m.edit.value, "hi")
	}

	next, _ = m.handleKey(tea.KeyMsg{Type: tea.KeyBackspace})
	m = next.(model)
	if m.edit.value != "h" {
		t.Fatalf("edit.value after backspace = %q, want %q", m.edit.value, "h")
	}

	next, _ = m.handleKey(tea.KeyMsg{Type: tea.KeyEsc})
	m = next.(model)
	if m.edit.kind != editNone {
		t.Errorf("edit.kind after esc = %v, want editNone", m.edit.kind)
	}
	if !strings.Contains(m.notice, "canceled") {
		t.Errorf("notice after esc = %q, want mention of canceled", m.notice)
	}
}

func TestHandleEditKey_BackspaceOnEmptyIsNoop(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.edit = editState{kind: editURL, value: ""}
	next, _ := m.handleKey(tea.KeyMsg{Type: tea.KeyBackspace})
	m = next.(model)
	if m.edit.value != "" {
		t.Errorf("edit.value after backspace on empty = %q, want empty", m.edit.value)
	}
}

func TestCommitEdit_URL(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.edit = editState{kind: editURL, field: "url", value: "  http://example.test:9  "}
	m.commitEdit()
	if m.cfg.BaseURL != "http://example.test:9" {
		t.Errorf("BaseURL after commit = %q, want trimmed URL", m.cfg.BaseURL)
	}
	if m.edit.kind != editNone {
		t.Error("edit state should be cleared after commit")
	}

	prevURL := m.cfg.BaseURL
	m.edit = editState{kind: editURL, field: "url", value: "   "}
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
	}
	for _, c := range cases {
		t.Run(c.field+"_"+c.value, func(t *testing.T) {
			m := newModel(goclient.DefaultConfig())
			before := c.check(m.cfg)
			m.edit = editState{kind: editDuration, field: c.field, value: c.value}
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
			m.edit = editState{kind: editInt, value: c.value}
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
	m.edit = editState{kind: editInt, field: "auto-streams", value: "1"}
	m.commitEdit()
	if m.cfg.TransferStreams.AutomaticMax != 1 {
		t.Fatalf("automatic max = %d, want 1", m.cfg.TransferStreams.AutomaticMax)
	}
	m.edit = editState{kind: editInt, field: "auto-streams", value: "0"}
	m.commitEdit()
	if m.cfg.TransferStreams.AutomaticMax != 1 {
		t.Fatalf("invalid automatic max changed to %d", m.cfg.TransferStreams.AutomaticMax)
	}
}

func TestHandleKey_RunMode_CancelAndMenuReturn(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.mode = modeRun
	canceled := false
	m.cancel = func() { canceled = true }

	next, _ := m.handleKey(tea.KeyMsg{Type: tea.KeyEsc})
	m = next.(model)
	if !canceled {
		t.Error("esc during run should invoke cancel")
	}
	if m.status != "canceling" {
		t.Errorf("status after cancel = %q, want canceling", m.status)
	}

	m.complete = true
	next, _ = m.handleKey(keyRunes("m"))
	m = next.(model)
	if m.mode != modeConfigure || m.section != sectionRun || m.row != 0 {
		t.Errorf("after 'm' on complete run: mode=%v section=%v row=%d", m.mode, m.section, m.row)
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
	if next.section != sectionStages || next.row != 0 {
		t.Errorf("section=%v row=%d, want sectionStages/0", next.section, next.row)
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
	pf := &wire.Preflight{Server: wire.ServerInfo{Name: "srv", Host: "host.example", Port: 8765, Location: "ams"}}
	m2.apply(goclient.Event{Kind: goclient.EventPreflight, Preflight: pf})
	if m2.status != "connected" || !strings.Contains(m2.server, "host.example") {
		t.Errorf("after EventPreflight: status=%q server=%q", m2.status, m2.server)
	}
}

// TestApply_DuplicateAndOutOfOrderEvents checks apply has no ordering guard:
// a Result straggling in after Complete is still recorded, a duplicate
// Complete/Error is a harmless no-op, and interleaved throughput samples for
// both directions track independent rates and peaks.
func TestApply_DuplicateAndOutOfOrderEvents(t *testing.T) {
	m := newModel(goclient.DefaultConfig())

	m.apply(goclient.Event{Kind: goclient.EventComplete})
	if !m.complete {
		t.Fatal("expected complete after EventComplete")
	}

	// A Result arriving after Complete (e.g. a straggling loaded-latency
	// sample) is still appended; apply doesn't gate on run state.
	m.apply(goclient.Event{Kind: goclient.EventResult, Result: &goclient.Result{Stage: "download"}})
	if len(m.results) != 1 {
		t.Errorf("late-arriving Result after Complete should still be recorded, got %d results", len(m.results))
	}

	// A duplicate Complete is a no-op.
	m.apply(goclient.Event{Kind: goclient.EventComplete})
	if !m.complete || m.status != "complete" {
		t.Errorf("duplicate EventComplete changed state unexpectedly: complete=%v status=%q", m.complete, m.status)
	}

	// A late Error still overwrites status/err; apply has no "already done"
	// guard against post-completion events.
	boom := errors.New("late boom")
	m.apply(goclient.Event{Kind: goclient.EventError, Err: boom})
	if m.err != boom || m.status != "error" {
		t.Errorf("late EventError not applied: err=%v status=%q", m.err, m.status)
	}

	// Interleaved throughput samples for both directions must not clobber
	// each other's rate or peak.
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

	got, cmd := m.Update(eventMsg(goclient.Event{
		Kind:   goclient.EventResult,
		Stage:  "bidirectional",
		Result: &goclient.Result{Stage: "bidirectional", Direction: goclient.Down, TotalBytes: 24},
	}))
	if cmd == nil {
		t.Fatal("completed runs must keep waiting for buffered events")
	}

	msg := cmd()
	if msg == nil {
		t.Fatal("waitEvent returned nil before draining the buffered upload result")
	}
	got, _ = got.Update(msg)
	mm := got.(model)

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

func TestThroughputSmoothingAndScale(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	// First sample seeds the EWMA; a second pulls it toward the new value but
	// not all the way (alpha 0.35), while peak/raw rate stay authoritative.
	m.apply(goclient.Event{Kind: goclient.EventThroughput, Direction: goclient.Down, Throughput: goclient.ThroughputSample{BytesPerSec: 100}})
	if m.smoothRate[goclient.Down] != 100 {
		t.Fatalf("first sample should seed EWMA to 100, got %v", m.smoothRate[goclient.Down])
	}
	m.apply(goclient.Event{Kind: goclient.EventThroughput, Direction: goclient.Down, Throughput: goclient.ThroughputSample{BytesPerSec: 200}})
	if got := m.smoothRate[goclient.Down]; got <= 100 || got >= 200 {
		t.Errorf("EWMA should sit between old and new sample, got %v", got)
	}
	// rateScale is the larger peak across both directions.
	m.apply(goclient.Event{Kind: goclient.EventThroughput, Direction: goclient.Up, Throughput: goclient.ThroughputSample{BytesPerSec: 50}})
	if got := m.rateScale(); got != m.peaks[goclient.Down] {
		t.Errorf("rateScale = %v, want the larger peak %v", got, m.peaks[goclient.Down])
	}
}

func TestAnimateEasesAndSettles(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.smoothRate[goclient.Down] = 1000
	// One frame moves partway (ease 0.2), not instantly to target.
	m.animate()
	if m.dispRate[goclient.Down] <= 0 || m.dispRate[goclient.Down] >= 1000 {
		t.Fatalf("after one frame dispRate should be between 0 and target, got %v", m.dispRate[goclient.Down])
	}
	if m.frame != 1 {
		t.Errorf("frame counter = %d, want 1", m.frame)
	}
	// Enough frames converge exactly to target, so settled() reports true.
	for i := 0; i < 200 && !m.settled(); i++ {
		m.animate()
	}
	if !m.settled() {
		t.Fatalf("dispRate did not settle: disp=%v target=%v", m.dispRate[goclient.Down], m.smoothRate[goclient.Down])
	}
}

func TestFrameMsgLoopStopsOutsideRun(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.mode = modeRun
	_, cmd := m.Update(frameMsg(time.Now()))
	if cmd == nil {
		t.Error("frameMsg during a run should reschedule the next frame")
	}
	m.mode = modeConfigure
	next, cmd := m.Update(frameMsg(time.Now()))
	if cmd != nil {
		t.Error("frameMsg outside a run should not reschedule")
	}
	if next.(model).animating {
		t.Error("leaving run mode should clear the animating flag")
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
	// Upload is half of download and shares the scale, so its bar must have fewer
	// filled cells than download's.
	down := strings.Count(firstLineContaining(out, "download"), "█")
	up := strings.Count(firstLineContaining(out, "upload"), "█")
	if !(down > up && up > 0) {
		t.Errorf("expected upload bar shorter than download on a shared scale: down=%d up=%d", down, up)
	}
}

func firstLineContaining(s, sub string) string {
	for _, line := range strings.Split(s, "\n") {
		if strings.Contains(line, sub) {
			return line
		}
	}
	return ""
}

func TestUpdate_WindowSize(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	got, _ := m.Update(tea.WindowSizeMsg{Width: 100, Height: 40})
	mm := got.(model)
	if mm.width != 100 || mm.height != 40 {
		t.Errorf("width=%d height=%d, want 100/40", mm.width, mm.height)
	}
}

// TestUpdate_WindowSizeDuringRun checks a resize mid-run only updates the
// layout dimensions, leaving the in-progress run's state (mode, stage,
// status, results) untouched and producing no follow-up cmd.
func TestUpdate_WindowSizeDuringRun(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.mode = modeRun
	m.stage = "download"
	m.status = "measure"
	m.results = []goclient.Result{{Stage: "latency"}}
	m.events = make(chan goclient.Event)

	got, cmd := m.Update(tea.WindowSizeMsg{Width: 120, Height: 45})
	mm := got.(model)
	if mm.width != 120 || mm.height != 45 {
		t.Errorf("width=%d height=%d, want 120/45", mm.width, mm.height)
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
	m := newModel(goclient.DefaultConfig())
	got, _ := m.Update(doneMsg{err: nil})
	mm := got.(model)
	if !mm.complete || mm.err != nil {
		t.Errorf("done with no error: complete=%v err=%v", mm.complete, mm.err)
	}

	m = newModel(goclient.DefaultConfig())
	boom := errors.New("boom")
	got, _ = m.Update(doneMsg{err: boom})
	mm = got.(model)
	if !mm.complete || mm.err != boom || mm.status != "error" {
		t.Errorf("done with error: complete=%v err=%v status=%q", mm.complete, mm.err, mm.status)
	}

	m = newModel(goclient.DefaultConfig())
	got, _ = m.Update(doneMsg{err: errors.New("context canceled")})
	mm = got.(model)
	if !mm.complete || mm.status != "canceled" || mm.err != nil {
		t.Errorf("done canceled: complete=%v status=%q err=%v", mm.complete, mm.status, mm.err)
	}
}

func TestUpdate_EventMsg(t *testing.T) {
	m := newModel(goclient.DefaultConfig())
	m.mode = modeRun
	m.events = make(chan goclient.Event)

	got, cmd := m.Update(eventMsg(goclient.Event{Kind: goclient.EventStage, Stage: "x"}))
	mm := got.(model)
	if mm.stage != "x" {
		t.Errorf("stage = %q, want x", mm.stage)
	}
	if cmd == nil {
		t.Error("expected a non-nil cmd to keep waiting for more events")
	}

	got, cmd = mm.Update(eventMsg(goclient.Event{Kind: goclient.EventComplete}))
	mm = got.(model)
	if !mm.complete {
		t.Error("expected complete after EventComplete")
	}
	if cmd == nil {
		t.Error("expected a non-nil cmd to drain buffered events after completion")
	}
}
