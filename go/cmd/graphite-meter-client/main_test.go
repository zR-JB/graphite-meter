package main

import (
	"crypto/tls"
	"crypto/x509"
	"errors"
	"fmt"
	"os"
	"strings"
	"syscall"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/quic-go/webtransport-go"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func modelAndCmd(next tea.Model, cmd tea.Cmd) (model, tea.Cmd) { return next.(model), cmd }

func press(name string) tea.KeyPressMsg {
	codes := map[string]tea.Key{
		"enter": {Code: tea.KeyEnter}, "esc": {Code: tea.KeyEscape}, "space": {Code: tea.KeySpace, Text: " "},
		"tab": {Code: tea.KeyTab}, "shift+tab": {Code: tea.KeyTab, Mod: tea.ModShift}, "up": {Code: tea.KeyUp},
		"down": {Code: tea.KeyDown}, "left": {Code: tea.KeyLeft}, "right": {Code: tea.KeyRight},
		"ctrl+c": {Code: 'c', Mod: tea.ModCtrl},
	}
	if k, ok := codes[name]; ok {
		return tea.KeyPressMsg(k)
	}
	return tea.KeyPressMsg{Code: []rune(name)[0], Text: name}
}

func view(m model) string { return m.View().Content }

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
		"medium":  goclient.PingMedium,
		"1500ms":  1500 * time.Millisecond,
		"80ms":    goclient.PingFast,
		"79ms":    0,
		"instant": 0,
		"0s":      0,
	} {
		got, err := parsePing(raw)
		if got != want || (err != nil) != (want == 0) {
			t.Errorf("parsePing(%q) = %v, %v; want %v", raw, got, err, want)
		}
	}
}

func TestCommandLineSettingsAndExitStatus(t *testing.T) {
	t.Parallel()
	for _, c := range []struct {
		edit func(*goclient.Config)
		want string
	}{
		{func(*goclient.Config) {}, ""},
		{func(c *goclient.Config) { c.Stages = goclient.StageSet{} }, "selects no stage"},
		{func(c *goclient.Config) { c.Warmup = -time.Second }, "Warmup must be"},
		{func(c *goclient.Config) { c.DownloadDuration = 0 }, "Download duration must be"},
		{func(c *goclient.Config) { c.BidirectionalDuration = time.Hour }, "Bidirectional duration must be"},
	} {
		cfg := goclient.DefaultConfig()
		c.edit(&cfg)
		if err := checkSettings(cfg); c.want == "" && err != nil || c.want != "" &&
			(err == nil || !strings.Contains(err.Error(), c.want)) {
			t.Errorf("checkSettings = %v, want %q", err, c.want)
		}
	}
	m := testModel(t)
	for _, c := range []struct {
		last        goclient.Outcome
		interrupted bool
		caught      any
		want        int
	}{
		{"", false, nil, 0},
		{goclient.OutcomeComplete, false, nil, 0},
		{goclient.OutcomePartial, false, nil, 1},
		{goclient.OutcomeIncomplete, false, nil, 1},
		{goclient.OutcomeStopped, false, nil, 1},
		{goclient.OutcomeFailed, false, nil, 1},
		{goclient.OutcomeComplete, true, nil, 130},
		{goclient.OutcomeComplete, false, os.Interrupt, 130},
		{goclient.OutcomeComplete, false, syscall.SIGTERM, 143},
	} {
		m.last, m.interrupted = c.last, c.interrupted
		if got := exitStatus(m, c.caught); got != c.want {
			t.Errorf("exit status after %q (interrupted %v, %v) = %d, want %d", c.last, c.interrupted, c.caught,
				got, c.want)
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
		fmtMs(99960 * time.Microsecond):     "100 ms",
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
		want  string
	}{
		{goclient.LatencyStats{}, nil, "— |  | — | — | — | 0 replies"},
		{goclient.LatencyStats{Timeouts: 3}, nil, "— |  | — | — | 3/3 (100.0%) | 0 replies"},
		{goclient.LatencyStats{Count: 2, JitterPairs: 1, P50: 12 * time.Millisecond, P95: 20 * time.Millisecond},
			&idle, "12.0 ms | +2.0 ms | 20.0 ms | 0.0 ms | 0/2 (0.0%) | 2 replies"},
		{goclient.LatencyStats{Count: 1, P50: 8 * time.Millisecond}, &idle, "8.0 ms | −2.0 ms"},
		{goclient.LatencyStats{Count: 999, Timeouts: 1}, nil, "1/1000 (0.10%)"},
		{goclient.LatencyStats{Unresolved: 2, SendFailures: 1, Elapsed: 4 * time.Second}, nil,
			"0 replies | 4.0 s | unfinished probes 2 | failed sends 1"},
	} {
		got := strings.Join(append(latencyCells(c.stats, c.idle), latencyFacts(c.stats)...), " | ")
		if !strings.Contains(got, c.want) {
			t.Errorf("summary %q, want %q", got, c.want)
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
		{1, 7, func(m model) bool { return m.edit != nil && m.edit.row == sections[1].rows[7] }, false},
		{2, 0, func(m model) bool { return m.cfg.PingInterval == 600*time.Millisecond }, true},
		{2, 1, func(m model) bool { return m.cfg.TransferStreams.Forced == 6 }, true},
		{2, 3, func(m model) bool { return m.cfg.InsecureSkipTLSVerify }, true},
		{0, 0, func(m model) bool { return m.edit != nil && m.edit.row == catalogueRow }, false},
	} {
		m := testModel(t)
		m.section, m.row = c.section, c.row
		seq := m.prepareSeq
		m, cmd := modelAndCmd(m.Update(press("enter")))
		if !c.check(m) || (m.prepareSeq != seq) != c.recheck || c.recheck && cmd == nil {
			t.Errorf(
				"%s: config=%+v edit=%v rechecked=%v",
				sections[c.section].rows[c.row].row(m).label,
				m.cfg,
				m.edit != nil,
				m.prepareSeq != seq,
			)
		}
	}
}

func TestCommitEdit(t *testing.T) {
	t.Parallel()
	for _, c := range []struct {
		row     *setting
		forced  bool
		typed   string
		check   func(goclient.Config) bool
		wantErr string
	}{
		{
			catalogueRow,
			false,
			"meter.example:8443/",
			func(c goclient.Config) bool { return c.BaseURL == "https://meter.example:8443" },
			"",
		},
		{
			catalogueRow,
			false,
			"127.0.0.1:7247",
			func(c goclient.Config) bool { return c.BaseURL == "http://127.0.0.1:7247" },
			"",
		},
		{
			catalogueRow,
			false,
			"https://METER.example",
			func(c goclient.Config) bool { return c.BaseURL == "https://meter.example" },
			"",
		},
		{catalogueRow, false, "ftp://meter.example", nil, "http:// or https://"},
		{warmupRow, false, "0", func(c goclient.Config) bool { return c.Warmup == 0 }, ""},
		{
			sections[1].rows[7],
			false,
			"12",
			func(c goclient.Config) bool { return c.DownloadDuration == 12*time.Second },
			"",
		},
		{
			sections[1].rows[7],
			false,
			"1.5m",
			func(c goclient.Config) bool { return c.DownloadDuration == 90*time.Second },
			"",
		},
		{sections[1].rows[8], false, "0", nil, "from 500 ms to 300 s"},
		{sections[1].rows[8], false, "6m", nil, "from 500 ms to 300 s"},
		{warmupRow, false, "5s", nil, "from 0 ms to 4 s"},
		{sections[1].rows[8], false, "soon", nil, "duration like"},
		{streamsRow, false, "8", func(c goclient.Config) bool {
			return c.TransferStreams == goclient.TransferStreamPolicy{AutomaticMax: 8}
		}, ""},
		{streamsRow, true, "9", func(c goclient.Config) bool {
			return c.TransferStreams.Forced == 9 && c.TransferStreams.AutomaticMax == 6
		}, ""},
		{streamsRow, false, "129", nil, "1 to 128"},
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
			if m.edit == nil || !strings.Contains(m.edit.err, c.wantErr) || m.edit.input.Value() != c.typed {
				t.Errorf("%q: edit=%+v, want it open with %q", c.typed, m.edit, c.wantErr)
			}
			continue
		}
		if m.edit != nil || !c.check(m.cfg) {
			t.Errorf("%q: edit open=%v config=%+v", c.typed, m.edit != nil, m.cfg)
		}
	}
}

func TestEditKeysDiscardAndQuit(t *testing.T) {
	t.Parallel()
	m := testModel(t)
	m.beginEdit(catalogueRow, m.cfg.BaseURL)
	m, _ = modelAndCmd(m.Update(press("x")))
	if m, _ = modelAndCmd(m.Update(press("esc"))); m.edit != nil ||
		m.cfg.BaseURL != goclient.DefaultConfig().BaseURL {
		t.Fatal("esc applied the edit")
	}
	m.beginEdit(catalogueRow, "")
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
	pending := &goclient.PendingAuthorization{Code: "ABCD", BrowserURL: "https://meter.example/auth/cli"}
	m.auth = &signIn{pending: pending, since: time.Now()}
	m.prepare = prepareSignIn
	for _, k := range []string{"enter", "space", "o", "enter"} {
		m, _ = modelAndCmd(m.Update(press(k)))
	}
	if opened != 4 || m.edit != nil || m.auth == nil || !m.auth.opened {
		t.Fatalf("opened=%d edit=%v auth=%v", opened, m.edit != nil, m.auth)
	}
	for _, binding := range m.ShortHelp() {
		if binding.Help().Desc == keys.change.Help().Desc {
			t.Fatal("footer offered enter to a row while sign-in owns it")
		}
	}
	if screen := view(m); !strings.Contains(screen, "Waiting for approval…") ||
		!strings.Contains(screen, "open page") || !strings.Contains(screen, "ABCD") {
		t.Fatalf("sign-in popup: %q", screen)
	}
	seq := m.prepareSeq
	m, _ = modelAndCmd(m.Update(press("esc")))
	if m.auth != nil || m.prepareSeq == seq || m.statusLabel() != "Sign in" || !strings.Contains(m.notice, "v") {
		t.Fatalf("esc did not cancel sign-in: auth=%v prepare=%v", m.auth, m.prepare)
	}
	if m, _ = modelAndCmd(m.Update(press("r"))); m.run != nil {
		t.Fatal("r started a run that still needs sign-in")
	}
	m.auth = &signIn{pending: pending, since: time.Now()}
	m, _ = modelAndCmd(m.Update(authTokenMsg{seq: m.prepareSeq, err: goclient.ErrApprovalExpired}))
	if m.auth != nil || m.statusLabel() != "Sign in" || !strings.Contains(m.notice, "expired") {
		t.Fatalf("expiry reads %q / %q", m.statusLabel(), m.notice)
	}
}

func TestStaleRepliesAreDropped(t *testing.T) {
	t.Parallel()
	m := testModel(t)
	m.prepareSeq, m.runSeq = 5, 3
	before := view(m)
	for _, msg := range []tea.Msg{
		preparationMsg{seq: 4, err: errors.New("stale")},
		authChallengeMsg{seq: 4, pending: &goclient.PendingAuthorization{}},
		authTokenMsg{seq: 4, token: "grant"},
		eventsMsg{seq: 2, events: []goclient.Event{{Kind: goclient.EventDone}}},
		prepareDueMsg{seq: 4},
	} {
		next, cmd := modelAndCmd(m.Update(msg))
		if cmd != nil || view(next) != before || next.auth != nil || next.run != nil {
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
	m.run = newRunState(m.cfg, "", time.Now())
	m.run.err, m.run.outcome = remote, goclient.OutcomeFailed
	multi := runModel(t, "a", "b")
	failure := goclient.ServerFailure{ServerID: "b", Scope: "throughput", Err: remote}
	multi.run.details.Failures = []goclient.ServerFailure{failure}
	multi, _ = modelAndCmd(multi.Update(eventsMsg{seq: multi.runSeq, events: []goclient.Event{
		{Kind: goclient.EventServerFailure, ServerID: "b", Failure: &failure},
	}}))
	views := []string{view(failed), view(partial), view(m), m.finalReport(), view(multi), multi.detailsView(120, true)}
	for _, view := range views {
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
	m.auth = &signIn{pending: &goclient.PendingAuthorization{}}
	m, cmd := modelAndCmd(m.Update(authTokenMsg{seq: m.prepareSeq, token: "grant", origin: "https://a.example"}))
	if cmd == nil || m.prepare != prepareChecking || !strings.Contains(m.notice, "discarded") {
		t.Fatalf("a grant from another issuer was accepted: %q", m.notice)
	}
	m, cmd = modelAndCmd(m.Update(authTokenMsg{seq: m.prepareSeq, token: "grant", origin: "https://b.example"}))
	if cmd == nil || m.prepare != prepareChecking || !strings.Contains(m.notice, "Signed in") {
		t.Fatalf("the challenging issuer's grant was refused: %q", m.notice)
	}
}

func TestCertificateErrorsNameTheSkipSetting(t *testing.T) {
	t.Parallel()
	untrusted := &tls.CertificateVerificationError{Err: x509.UnknownAuthorityError{}}
	long := fmt.Errorf("%s: %w", strings.Repeat("path ", 80), untrusted)
	const hint = "Turn on Skip TLS verify (-insecure) only for a server you trust."
	if got := errorText(long); !strings.HasSuffix(got, hint) {
		t.Fatalf("certificate failure hides the skip setting: %q", got)
	}
	if got := errorText(errors.New("refused")); strings.Contains(got, "Skip TLS") {
		t.Fatalf("an unrelated failure suggests skipping verification: %q", got)
	}
}
