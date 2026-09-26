package main

import (
	"errors"
	"strings"
	"testing"
	"time"

	tea "github.com/charmbracelet/bubbletea"
	"github.com/quic-go/webtransport-go"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func modelAndCmd(next tea.Model, cmd tea.Cmd) (model, tea.Cmd) { return next.(model), cmd }

func press(name string) tea.KeyMsg {
	types := map[string]tea.KeyType{
		"enter": tea.KeyEnter, "esc": tea.KeyEsc, "space": tea.KeySpace, "tab": tea.KeyTab,
		"shift+tab": tea.KeyShiftTab, "up": tea.KeyUp, "down": tea.KeyDown, "left": tea.KeyLeft,
		"right": tea.KeyRight, "ctrl+c": tea.KeyCtrlC,
	}
	if t, ok := types[name]; ok {
		return tea.KeyMsg{Type: t}
	}
	return tea.KeyMsg{Type: tea.KeyRunes, Runes: []rune(name)}
}

func quits(cmd tea.Cmd) bool {
	if cmd == nil {
		return false
	}
	_, ok := cmd().(tea.QuitMsg)
	return ok
}

func testModel(t *testing.T) model {
	t.Helper()
	m := newModel(goclient.DefaultConfig())
	t.Cleanup(m.close)
	m.width, m.height = 120, 40
	m.prepare = prepareReady
	return m
}

func readyConnection(name string) *goclient.PreparedConnection {
	return &goclient.PreparedConnection{
		ThroughputTarget: wire.ThroughputTarget{
			Origin:    "https://" + name + ".example",
			Transport: wire.TransportFetchStream,
			Protocol:  "http2",
		},
		LatencyTarget: &wire.LatencyTarget{
			Origin:    "https://" + name + ".example",
			Transport: wire.TransportWebSocket,
			Protocol:  "http1",
		},
	}
}

func preparedFixture(errs ...error) *goclient.PreparedRun {
	run := &goclient.PreparedRun{}
	for i, err := range errs {
		id := string(rune('a' + i))
		server := wire.ServerEntry{ID: id, Name: strings.ToUpper(id), URL: "https://" + id + ".example"}
		run.Catalog.Servers = append(run.Catalog.Servers, server)
		prepared := goclient.PreparedServer{Server: server, Err: err}
		if err == nil {
			prepared.Connection = readyConnection(id)
		}
		run.Servers = append(run.Servers, prepared)
	}
	return run
}

func TestParseStages(t *testing.T) {
	t.Parallel()
	for raw, want := range map[string]goclient.StageSet{
		"latency,download,upload,bidirectional": {Latency: true, Download: true, Upload: true, Bidirectional: true},
		"ping,down,up,bidi":                     {Latency: true, Download: true, Upload: true, Bidirectional: true},
		"":                                      {},
		"download,bogus":                        {Download: true},
		" Latency , DOWN ":                      {Latency: true, Download: true},
	} {
		if got := parseStages(raw); got != want {
			t.Errorf("parseStages(%q) = %+v, want %+v", raw, got, want)
		}
	}
}

func TestParsePing(t *testing.T) {
	t.Parallel()
	for raw, want := range map[string]time.Duration{
		"fast":    80 * time.Millisecond,
		"Slow":    600 * time.Millisecond,
		"":        250 * time.Millisecond,
		"1500ms":  1500 * time.Millisecond,
		"instant": 0,
		"0s":      0,
	} {
		got, err := parsePing(raw)
		if got != want || (err != nil) != (want == 0) {
			t.Errorf("parsePing(%q) = %v, %v; want %v", raw, got, err, want)
		}
	}
}

func TestFormatting(t *testing.T) {
	t.Parallel()
	for got, want := range map[string]string{
		fmtRate(0):                          "0.00 bit/s",
		fmtRate(1500):                       "12.00 kbit/s",
		fmtRate(12_500_000):                 "100.0 Mbit/s",
		fmtRate(137_500_000):                "1100 Mbit/s",
		fmtRate(162_500_000):                "1.30 Gbit/s",
		fmtBytes(999):                       "999 B",
		fmtBytes(1_500):                     "1.5 kB",
		fmtBytes(2_340_000_000):             "2.3 GB",
		fmtMs(12345 * time.Microsecond):     "12.3 ms",
		fmtMs(123456 * time.Microsecond):    "123 ms",
		fmtAdded(-1200 * time.Microsecond):  "−1.2 ms",
		fmtAdded(7800 * time.Microsecond):   "+7.8 ms",
		fmtSetting(800 * time.Millisecond):  "800 ms",
		fmtSetting(1500 * time.Millisecond): "1.5 s",
		fmtSetting(10 * time.Second):        "10 s",
	} {
		if got != want {
			t.Errorf("got %q, want %q", got, want)
		}
	}
}

func TestLatencySummaryVocabulary(t *testing.T) {
	t.Parallel()
	idle := goclient.LatencyStats{Count: 4, P50: 10 * time.Millisecond}
	for _, c := range []struct {
		stats goclient.LatencyStats
		idle  *goclient.LatencyStats
		want  []string
	}{
		{goclient.LatencyStats{}, nil, []string{"median —", "p95 —", "jitter —", "probe timeouts —", "0 replies"}},
		{goclient.LatencyStats{Timeouts: 3}, nil, []string{"probe timeouts 3/3 (100.0%)"}},
		{
			goclient.LatencyStats{Count: 2, JitterPairs: 1, P50: 12 * time.Millisecond, P95: 20 * time.Millisecond},
			&idle,
			[]string{"median 12.0 ms", "+2.0 ms added", "p95 20.0 ms", "jitter 0.0 ms"},
		},
		{goclient.LatencyStats{Count: 1, P50: 8 * time.Millisecond}, &idle, []string{"−2.0 ms added"}},
		{
			goclient.LatencyStats{Unresolved: 2, SendFailures: 1, Elapsed: 4 * time.Second},
			nil,
			[]string{"4.0 s", "unfinished probes 2", "failed sends 1"},
		},
	} {
		got := strings.Join(latencyParts(c.stats, c.idle), " · ")
		for _, want := range c.want {
			if !strings.Contains(got, want) || strings.Contains(strings.ToLower(got), "loss") {
				t.Errorf("summary %q, want %q", got, want)
			}
		}
	}
}

func TestNavigationWrapsSectionsAndClampsRows(t *testing.T) {
	t.Parallel()
	m := testModel(t)
	for _, step := range []struct {
		key          string
		section, row int
	}{
		{"shift+tab", 2, 0}, {"tab", 0, 0}, {"up", 0, 0},
		{"down", 0, 1}, {"j", 0, 2}, {"right", 1, 2}, {"left", 0, 2},
		{"tab", 1, 2}, {"tab", 2, 2}, {"down", 2, 3}, {"down", 2, 4}, {"down", 2, 4},
	} {
		m, _ = modelAndCmd(m.Update(press(step.key)))
		if m.section != step.section || m.row != step.row {
			t.Fatalf("after %s: section=%d row=%d, want %d/%d", step.key, m.section, m.row, step.section, step.row)
		}
	}
}

func TestRowActivation(t *testing.T) {
	t.Parallel()
	for _, c := range []struct {
		section, row int
		check        func(model) bool
		recheck      bool
	}{
		{1, 3, func(m model) bool { return m.cfg.Stages.Bidirectional }, true},
		{1, 4, func(m model) bool { return !m.cfg.LoadedLatency }, true},
		{1, 7, func(m model) bool { return m.edit.row != nil && *m.edit.row == rowDownloadDuration }, false},
		{2, 0, func(m model) bool { return m.cfg.PingInterval == 600*time.Millisecond }, true},
		{2, 1, func(m model) bool { return m.cfg.TransferStreams.Forced == 6 }, true},
		{2, 3, func(m model) bool { return m.cfg.InsecureSkipTLSVerify }, true},
		{0, 0, func(m model) bool { return m.edit.row != nil && *m.edit.row == rowCatalogue }, false},
	} {
		m := testModel(t)
		m.section, m.row = c.section, c.row
		seq := m.prepareSeq
		m, cmd := modelAndCmd(m.Update(press("enter")))
		if !c.check(m) || (m.prepareSeq != seq) != c.recheck || c.recheck && cmd == nil {
			t.Errorf(
				"%s: config=%+v edit=%v rechecked=%v",
				m.setupRow(sections[c.section].rows[c.row]).label,
				m.cfg,
				m.edit.row != nil,
				m.prepareSeq != seq,
			)
		}
	}
}

func TestCommitEdit(t *testing.T) {
	t.Parallel()
	for _, c := range []struct {
		row     rowID
		forced  bool
		typed   string
		check   func(goclient.Config) bool
		wantErr string
	}{
		{
			rowCatalogue,
			false,
			"meter.example:8443/",
			func(c goclient.Config) bool { return c.BaseURL == "http://meter.example:8443" },
			"",
		},
		{
			rowCatalogue,
			false,
			"https://METER.example",
			func(c goclient.Config) bool { return c.BaseURL == "https://meter.example" },
			"",
		},
		{rowCatalogue, false, "ftp://meter.example", nil, "http:// or https://"},
		{rowWarmup, false, "0", func(c goclient.Config) bool { return c.Warmup == 0 }, ""},
		{
			rowDownloadDuration,
			false,
			"12",
			func(c goclient.Config) bool { return c.DownloadDuration == 12*time.Second },
			"",
		},
		{
			rowDownloadDuration,
			false,
			"1.5m",
			func(c goclient.Config) bool { return c.DownloadDuration == 90*time.Second },
			"",
		},
		{rowUploadDuration, false, "0", nil, "greater than zero"},
		{rowUploadDuration, false, "soon", nil, "duration like"},
		{rowStreams, false, "8", func(c goclient.Config) bool {
			return c.TransferStreams == goclient.TransferStreamPolicy{AutomaticMax: 8}
		}, ""},
		{rowStreams, true, "9", func(c goclient.Config) bool {
			return c.TransferStreams.Forced == 9 && c.TransferStreams.AutomaticMax == 6
		}, ""},
		{rowStreams, false, "129", nil, "1 to 128"},
	} {
		m := testModel(t)
		if c.forced {
			m.cfg.TransferStreams.Forced = 1
		}
		m.beginEdit(c.row, "")
		for _, r := range c.typed {
			m, _ = modelAndCmd(m.Update(press(string(r))))
		}
		m, _ = modelAndCmd(m.Update(press("enter")))
		if c.wantErr != "" {
			if m.edit.row == nil || !strings.Contains(m.edit.err, c.wantErr) || m.edit.input.Value() != c.typed {
				t.Errorf("%q: edit=%+v, want it open with %q", c.typed, m.edit, c.wantErr)
			}
			continue
		}
		if m.edit.row != nil || !c.check(m.cfg) {
			t.Errorf("%q: edit open=%v config=%+v", c.typed, m.edit.row != nil, m.cfg)
		}
	}
}

func TestEditKeysDiscardAndQuit(t *testing.T) {
	t.Parallel()
	m := testModel(t)
	m.beginEdit(rowCatalogue, m.cfg.BaseURL)
	m, _ = modelAndCmd(m.Update(press("x")))
	if m, _ = modelAndCmd(m.Update(press("esc"))); m.edit.row != nil ||
		m.cfg.BaseURL != goclient.DefaultConfig().BaseURL {
		t.Fatal("esc applied the edit")
	}
	m.beginEdit(rowCatalogue, "")
	if m, cmd := modelAndCmd(m.Update(press("q"))); quits(cmd) || m.edit.input.Value() != "q" {
		t.Fatal("q left the editor")
	}
	if _, cmd := modelAndCmd(m.Update(press("ctrl+c"))); !quits(cmd) {
		t.Fatal("ctrl+c did not quit from the editor")
	}
}

func TestSignInKeysOwnEnter(t *testing.T) {
	t.Parallel()
	m := testModel(t)
	opened := 0
	m.openApproval = func(*goclient.PendingAuthorization) { opened++ }
	m.auth = &goclient.PendingAuthorization{Code: "ABCD", BrowserURL: "https://meter.example/auth/cli"}
	m.prepare = prepareSignIn
	for _, k := range []string{"enter", "space", "o", "enter"} {
		m, _ = modelAndCmd(m.Update(press(k)))
	}
	if opened != 4 || m.edit.row != nil || m.auth == nil || !m.authOpened {
		t.Fatalf("opened=%d edit=%v auth=%v", opened, m.edit.row != nil, m.auth)
	}
	for _, binding := range m.ShortHelp() {
		if binding.Help().Desc == keys.change.Help().Desc {
			t.Fatal("footer offered enter to a row while sign-in owns it")
		}
	}
	if !strings.Contains(m.View(), "Waiting for approval…") || !strings.Contains(m.View(), "Open sign-in page") {
		t.Fatalf("sign-in panel: %q", m.View())
	}
	seq := m.prepareSeq
	m, _ = modelAndCmd(m.Update(press("esc")))
	if m.auth != nil || m.prepareSeq == seq || m.prepare != prepareFailed {
		t.Fatalf("esc did not cancel sign-in: auth=%v prepare=%v", m.auth, m.prepare)
	}
}

func TestStaleRepliesAreDropped(t *testing.T) {
	t.Parallel()
	m := testModel(t)
	m.prepareSeq, m.runSeq = 5, 3
	before := m.View()
	for _, msg := range []tea.Msg{
		preparationMsg{seq: 4, err: errors.New("stale")},
		authChallengeMsg{seq: 4, pending: &goclient.PendingAuthorization{}},
		authTokenMsg{seq: 4, token: "grant"},
		eventsMsg{seq: 2, events: []goclient.Event{{Kind: goclient.EventDone}}},
		prepareDueMsg{seq: 4},
	} {
		next, cmd := modelAndCmd(m.Update(msg))
		if cmd != nil || next.View() != before || next.auth != nil || next.run != nil {
			t.Fatalf("stale %T changed the model", msg)
		}
	}
}

func TestRemoteErrorsCannotWriteTerminalControls(t *testing.T) {
	t.Parallel()
	remote := &webtransport.SessionError{Remote: true, Message: "closed\x1b]52;c;cHduZWQ=\a\u009b2J\r"}
	m := testModel(t)
	m.prepareSeq = 1
	failed, _ := modelAndCmd(m.Update(preparationMsg{seq: 1, err: remote}))
	partial, _ := modelAndCmd(m.Update(preparationMsg{seq: 1, run: preparedFixture(nil, remote), err: remote}))
	m.run = newRunState(m.cfg, "")
	m.run.err, m.run.outcome = remote, goclient.OutcomeFailed
	for _, view := range []string{failed.View(), partial.View(), m.View(), m.finalReport()} {
		if !strings.Contains(view, "closed") || strings.ContainsAny(view, "\a\r\u009b") ||
			strings.Contains(view, "\x1b]") {
			t.Fatalf("remote error reached the terminal unfiltered: %q", view)
		}
	}
}

func TestAuthTokenIsBoundToTheChallengingIssuer(t *testing.T) {
	t.Parallel()
	m := testModel(t)
	m.preparedRun = preparedFixture(nil, &goclient.AuthRequiredError{})
	m.authServerID = "b"
	m.auth = &goclient.PendingAuthorization{}
	m, cmd := modelAndCmd(m.Update(authTokenMsg{seq: m.prepareSeq, token: "grant", origin: "https://a.example"}))
	if cmd == nil || m.prepare != prepareChecking || !strings.Contains(m.notice, "discarded") {
		t.Fatalf("a grant from another issuer was accepted: %q", m.notice)
	}
	m, cmd = modelAndCmd(m.Update(authTokenMsg{seq: m.prepareSeq, token: "grant", origin: "https://b.example"}))
	if cmd == nil || m.prepare != prepareChecking || !strings.Contains(m.notice, "Signed in") {
		t.Fatalf("the challenging issuer's grant was refused: %q", m.notice)
	}
}
