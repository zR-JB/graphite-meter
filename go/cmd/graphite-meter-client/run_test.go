package main

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func hangingServer(t *testing.T) (url string, entered, left <-chan struct{}) {
	t.Helper()
	in, out := make(chan struct{}, 16), make(chan struct{}, 16)
	srv := httptest.NewServer(http.HandlerFunc(func(_ http.ResponseWriter, r *http.Request) {
		in <- struct{}{}
		<-r.Context().Done()
		out <- struct{}{}
	}))
	t.Cleanup(srv.Close)
	return srv.URL, in, out
}

func within(t *testing.T, ch <-chan struct{}, what string) {
	t.Helper()
	select {
	case <-ch:
	case <-time.After(2 * time.Second):
		t.Fatal(what)
	}
}

func TestReprepareCancelsActiveRequestAndDiscardsItsReply(t *testing.T) {
	t.Parallel()
	url, entered, left := hangingServer(t)
	cfg := goclient.DefaultConfig()
	cfg.BaseURL = url
	m := newModel(cfg)
	t.Cleanup(m.close)
	m, command := modelAndCmd(m.Update(prepareDueMsg{seq: m.prepareSeq}))
	replied := make(chan tea.Msg, 1)
	go func() { replied <- command() }()
	within(t, entered, "preparation never reached the server")
	m.cfg.BaseURL = "http://new-server.test"
	m, _ = modelAndCmd(m.reprepare())
	within(t, left, "superseded preparation kept its HTTP request alive")
	reply := (<-replied).(preparationMsg)
	if !errors.Is(reply.err, context.Canceled) {
		t.Fatalf("superseded preparation = %v, want cancellation", reply.err)
	}
	if m, next := modelAndCmd(m.Update(reply)); next != nil || m.prepare != prepareChecking || m.prepareErr != "" {
		t.Fatalf("stale reply changed the new preparation: %+v", m.prepare)
	}
}

func TestQuitAndShutdownCancelOwnedWork(t *testing.T) {
	t.Parallel()
	for _, editing := range []bool{false, true} {
		m := newModel(goclient.DefaultConfig())
		preparation := m.preparation
		if editing {
			m.beginEdit(catalogueRow, m.cfg.BaseURL)
		}
		if _, cmd := modelAndCmd(m.Update(press("ctrl+c"))); !quits(cmd) {
			t.Fatal("ctrl+c did not quit")
		}
		for _, p := range []*goclient.Preparation{preparation, m.controller.NewPreparation(m.cfg)} {
			if _, err := p.PrepareRun(); !errors.Is(err, context.Canceled) {
				t.Fatalf("quit left preparation work possible: %v", err)
			}
		}
	}
	initial := newModel(goclient.DefaultConfig())
	updated, _ := modelAndCmd(initial.reprepare())
	initial.controller.Close()
	if _, err := updated.preparation.PrepareRun(); !errors.Is(err, context.Canceled) {
		t.Fatal("program exit did not cancel work created by an updated model")
	}
}

func TestPreparationCancellationReachesQueuedApprovalPoll(t *testing.T) {
	t.Parallel()
	cfg := goclient.DefaultConfig()
	cfg.BaseURL = "https://meter.test"
	m := newModel(cfg)
	t.Cleanup(m.close)
	pending, err := m.preparation.BeginAuthorization("", cfg.BaseURL+"/login")
	if err != nil {
		t.Fatal(err)
	}
	m.prepare = prepareSignIn
	_, poll := modelAndCmd(m.Update(authChallengeMsg{seq: m.prepareSeq, pending: pending}))
	m, _ = modelAndCmd(m.reprepare())
	for _, msg := range drain(poll) {
		if reply, ok := msg.(authTokenMsg); ok {
			if !errors.Is(reply.err, context.Canceled) || reply.token != "" {
				t.Fatalf("canceled approval = %+v", reply)
			}
			if _, next := modelAndCmd(m.Update(reply)); next != nil {
				t.Fatal("stale approval launched more work")
			}
			return
		}
	}
	t.Fatal("approval poll never replied")
}

func drain(cmd tea.Cmd) []tea.Msg {
	var out []tea.Msg
	switch msg := cmd().(type) {
	case tea.BatchMsg:
		for _, c := range msg {
			if c != nil {
				out = append(out, drain(c)...)
			}
		}
	default:
		out = append(out, msg)
	}
	return out
}

func TestStartingRunCancelsPreparationAndItsQueuedMessages(t *testing.T) {
	t.Parallel()
	url, entered, left := hangingServer(t)
	cfg := goclient.DefaultConfig()
	cfg.BaseURL = url
	m := newModel(cfg)
	t.Cleanup(m.close)
	preparation, seq := m.preparation, m.prepareSeq
	m, _ = modelAndCmd(m.startRun())
	if _, err := preparation.PrepareRun(); !errors.Is(err, context.Canceled) || m.prepareSeq == seq {
		t.Fatal("starting a run left its preparation active")
	}
	if _, cmd := modelAndCmd(m.Update(prepareDueMsg{seq: seq})); cmd != nil {
		t.Fatal("a queued preparation started during the run")
	}
	within(t, entered, "run never reached preparation")
	m, _ = modelAndCmd(m.Update(press("esc")))
	if !m.stopPrompt {
		t.Fatal("esc did not ask before stopping")
	}
	m, _ = modelAndCmd(m.Update(press("esc")))
	within(t, left, "stopping left the run's request alive")
	if done := finishFrom(t, m); done.run.outcome != goclient.OutcomeStopped || done.statusLabel() != "Stopped" {
		t.Fatalf("stopped run reads %q", done.statusLabel())
	}
}

func finishFrom(t *testing.T, m model) model {
	t.Helper()
	deadline := time.After(5 * time.Second)
	for m.running() {
		select {
		case <-deadline:
			t.Fatal("run never finished")
		default:
		}
		if msg := waitEvents(m.runSeq, m.events)(); msg != nil {
			m, _ = modelAndCmd(m.Update(msg))
		}
	}
	return m
}

func TestRunKeys(t *testing.T) {
	t.Parallel()
	m := testModel(t)
	m.run = newRunState(m.cfg, "", time.Now())
	m, _ = modelAndCmd(m.Update(press("x")))
	if m, _ = modelAndCmd(m.Update(press("esc"))); !m.stopPrompt {
		t.Fatal("esc did not ask before stopping")
	}
	if m, _ = modelAndCmd(m.Update(press("x"))); m.stopPrompt || !m.running() || m.notice != "Test continues." {
		t.Fatal("another key did not continue the test")
	}
	if m, _ = modelAndCmd(m.Update(press("r"))); m.runSeq != 0 {
		t.Fatal("r restarted a running test")
	}
	m.run.outcome = goclient.OutcomeComplete
	if m, _ = modelAndCmd(m.Update(press("esc"))); m.run != nil {
		t.Fatal("esc did not return to setup after the test")
	}
}

func runModel(t *testing.T, servers ...string) model {
	t.Helper()
	m := testModel(t)
	m.cfg.Stages.Bidirectional = true
	m.run = newRunState(m.cfg, "", time.Now())
	details := &goclient.RunDetails{LatencyFocus: servers[0], Participants: servers, Outcome: goclient.OutcomeRunning}
	for _, id := range servers {
		details.Servers = append(details.Servers, goclient.ServerRunSummary{
			Server:        wire.ServerEntry{ID: id, Name: strings.ToUpper(id), Location: "Somewhere"},
			Throughput:    wire.ThroughputTarget{Origin: "https://" + id, Transport: wire.TransportFetchStream, Protocol: "http2"},
			LatencyTarget: &wire.LatencyTarget{Origin: "https://" + id, Transport: wire.TransportWebSocket, Protocol: "http1"},
		})
	}
	m, _ = modelAndCmd(m.Update(eventsMsg{
		seq:    m.runSeq,
		events: []goclient.Event{{Kind: goclient.EventServers, Servers: details}},
	}))
	return m
}

func withPopulation(details *goclient.RunDetails, id string, results ...goclient.Result) {
	for i := range details.Servers {
		if details.Servers[i].Server.ID == id {
			details.Servers[i].Results = append(details.Servers[i].Results, results...)
		}
	}
}

func TestStatusLabelsFollowTheLifecycle(t *testing.T) {
	t.Parallel()
	m := runModel(t, "a")
	for _, c := range []struct {
		event goclient.Event
		want  string
	}{
		{
			goclient.Event{Kind: goclient.EventStage, Stage: goclient.StageLatency, Phase: goclient.PhasePreparing},
			"Checking paths",
		},
		{
			goclient.Event{Kind: goclient.EventStage, Stage: goclient.StageDownload, Phase: goclient.PhaseWarmup},
			"Warmup",
		},
		{
			goclient.Event{
				Kind:  goclient.EventStage,
				Stage: goclient.StageBidirectional,
				Phase: goclient.PhaseMeasuring,
			},
			"Bidirectional",
		},
	} {
		m, _ = modelAndCmd(m.Update(eventsMsg{seq: m.runSeq, events: []goclient.Event{c.event}}))
		if got := m.statusLabel(); got != c.want {
			t.Errorf("after %v/%v the header reads %q, want %q", c.event.Stage, c.event.Phase, got, c.want)
		}
	}
	for _, c := range []struct {
		done goclient.Event
		want string
	}{
		{
			goclient.Event{Kind: goclient.EventDone, Servers: &goclient.RunDetails{Outcome: goclient.OutcomePartial}},
			"Partial",
		},
		{goclient.Event{Kind: goclient.EventDone, Err: fmt.Errorf("stage: %w", context.Canceled)}, "Stopped"},
		{goclient.Event{Kind: goclient.EventDone, Err: errors.New("server said context canceled")}, "Failed"},
		{
			goclient.Event{
				Kind:    goclient.EventDone,
				Err:     errors.New("lost"),
				Servers: &goclient.RunDetails{Outcome: goclient.OutcomeIncomplete},
			},
			"Incomplete",
		},
	} {
		finished := runModel(t, "a")
		finished, _ = modelAndCmd(finished.Update(eventsMsg{seq: finished.runSeq, events: []goclient.Event{c.done}}))
		if got := finished.statusLabel(); got != c.want || strings.Contains(view(finished), "canceling") {
			t.Errorf("outcome %v reads %q, want %q", c.done.Outcome(), got, c.want)
		}
	}
	setup := testModel(t)
	for state, want := range map[prepareState]string{
		prepareChecking: "Checking paths",
		prepareSignIn:   "Sign in",
		prepareFailed:   "Path failed",
		prepareReady:    "Recheck needed",
	} {
		setup.prepare = state
		if got := setup.statusLabel(); got != want {
			t.Errorf("setup %v reads %q, want %q", state, got, want)
		}
	}
}

func TestTerminalEventKeepsBufferedResults(t *testing.T) {
	t.Parallel()
	m := runModel(t, "a")
	failure := errors.New("transfer failed")
	partial := &goclient.RunDetails{
		Servers:      m.run.details.Servers,
		Participants: []string{"a"},
		Outcome:      goclient.OutcomeIncomplete,
	}
	withPopulation(partial, "a", goclient.Result{
		Stage:   goclient.StageBidirectional,
		Latency: goclient.LatencyStats{Unresolved: 2},
		Err:     failure,
	})
	m, cmd := modelAndCmd(m.Update(eventsMsg{seq: m.runSeq, events: []goclient.Event{
		{Kind: goclient.EventStage, Stage: goclient.StageBidirectional, Phase: goclient.PhaseMeasuring},
		{
			Kind:      goclient.EventResult,
			Stage:     goclient.StageBidirectional,
			Direction: goclient.Down,
			Result: &goclient.Result{
				Stage:       goclient.StageBidirectional,
				Direction:   goclient.Down,
				TotalBytes:  42,
				Unavailable: true,
				Err:         failure,
			},
		},
		{Kind: goclient.EventDone, Err: failure, Servers: partial},
	}}))
	if cmd != nil ||
		!m.finished() ||
		!errors.Is(m.run.err, failure) ||
		len(m.run.results) != 1 ||
		m.run.stages[3].state != stageStopped {
		t.Fatalf("terminal state: %+v", m.run)
	}
	screen := view(m)
	for _, want := range []string{
		"Bi-dir ↓ incomplete: transfer failed",
		"Loaded latency · Bi-dir",
		"unfinished probes 2",
		"Bi-dir ↓",
		missing,
	} {
		if !strings.Contains(screen, want) {
			t.Errorf("view lost %q:\n%s", want, screen)
		}
	}
}

func TestResultsNameEveryPopulation(t *testing.T) {
	t.Parallel()
	m := runModel(t, "a")
	m.run.results = []goclient.Result{
		{
			Stage: goclient.StageDownload, Direction: goclient.Down, MeanBps: 117_500_000, PeakBps: 125_000_000,
			TotalBytes: 1_200_000_000, Elapsed: 10 * time.Second, Samples: 38,
		},
		{
			Stage: goclient.StageUpload, Direction: goclient.Up, MeanBps: 5_000_000,
			TotalBytes: 50_000_000, Elapsed: 10 * time.Second, Samples: 39,
		},
	}
	idle := goclient.LatencyStats{
		Count: 16, P50: 10 * time.Millisecond, P95: 12 * time.Millisecond, JitterPairs: 15,
		Jitter: 400 * time.Microsecond, Elapsed: 4 * time.Second,
		ReflectorTiming: &goclient.ReflectorTimingStats{
			Count: 2, MeanRawRTT: 10 * time.Millisecond, MeanAdjustedRTT: 10 * time.Millisecond,
		},
	}
	loaded := goclient.LatencyStats{Count: 40, P50: 17800 * time.Microsecond, P95: 30 * time.Millisecond, Timeouts: 2}
	withPopulation(m.run.details, "a",
		goclient.Result{Stage: goclient.StageLatency, Latency: idle},
		goclient.Result{Stage: goclient.StageDownload, Latency: loaded})
	m.run.outcome = goclient.OutcomeComplete
	text := m.finalReport()
	for _, want := range []string{
		"Graphite Meter · Complete", "Median", "Probe timeouts",
		"Idle latency", "10.0 ms", "12.0 ms", "0.4 ms", "Idle latency: 16 replies · 4.0 s",
		"Server timing (2 paired replies, means): raw 10.0 ms · handling 0.0 ms · adjusted 10.0 ms",
		"940.0 Mbit/s", "Download: peak 1000 Mbit/s · 1.2 GB · 10.0 s · 38 samples",
		"17.8 ms", "+7.8 ms", "2/42 (4.8%)", "Loaded latency · Download: 40 replies",
		"40.00 Mbit/s", "receiver-timed",
	} {
		if !strings.Contains(text, want) {
			t.Errorf("report lost %q:\n%s", want, text)
		}
	}
	if strings.Contains(text, "\x1b") || strings.Contains(strings.ToLower(text), "loss") {
		t.Errorf("report styling or vocabulary: %q", text)
	}
	m.run = nil
	if m.finalReport() != "" {
		t.Error("report before any run")
	}
}

func TestLiveViewFollowsTheStage(t *testing.T) {
	t.Parallel()
	m := runModel(t, "a")
	m.run.latest["a"] = goclient.LatencySample{RTT: 3 * time.Millisecond}
	for _, c := range []struct {
		stage         goclient.Stage
		want, without []string
	}{
		{goclient.StageLatency, []string{"Idle latency", "3.0 ms"}, []string{"↓", "↑"}},
		{goclient.StageDownload, []string{"↓", "Loaded latency"}, []string{"↑"}},
		{goclient.StageBidirectional, []string{"↓", "↑", "Loaded latency"}, nil},
	} {
		m, _ = modelAndCmd(m.Update(eventsMsg{seq: m.runSeq, events: []goclient.Event{
			{Kind: goclient.EventStage, Stage: c.stage, Phase: goclient.PhaseMeasuring, At: time.Now()},
			{Kind: goclient.EventThroughput, Direction: goclient.Down, At: time.Now(),
				Throughput: goclient.ThroughputSample{BytesPerSec: 1e6}},
			{Kind: goclient.EventLatency, ServerID: "a", At: time.Now(), Latency: goclient.LatencySample{RTT: 3e6}},
		}}))
		live := m.liveView(60, 16)
		for _, want := range c.want {
			if !strings.Contains(live, want) {
				t.Errorf("%s live view lost %q: %q", c.stage, want, live)
			}
		}
		for _, unwanted := range c.without {
			if strings.Contains(live, unwanted) {
				t.Errorf("%s live view drew %q: %q", c.stage, unwanted, live)
			}
		}
		if !strings.ContainsFunc(live, func(r rune) bool { return r > 0x2800 && r <= 0x28ff }) {
			t.Errorf("%s live view drew no chart: %q", c.stage, live)
		}
	}
}

func TestStageTrackFollowsStageEvents(t *testing.T) {
	t.Parallel()
	m := runModel(t, "a")
	start := time.Now()
	m.now = start.Add(2500 * time.Millisecond)
	m.apply(goclient.Event{
		Kind:  goclient.EventStage,
		Stage: goclient.StageLatency,
		Phase: goclient.PhaseFinished,
		At:    start,
	})
	m.apply(goclient.Event{
		Kind:  goclient.EventStage,
		Stage: goclient.StageDownload,
		Phase: goclient.PhaseMeasuring,
		At:    start,
	})
	m.apply(goclient.Event{
		Kind:  goclient.EventStage,
		Stage: goclient.StageUpload,
		Phase: goclient.Phase(99),
		At:    start,
	})
	track := ansi.Strip(strings.Join(m.stageTrack(80), "\n"))
	for _, want := range []string{"✓ 4 s", "2.5 s / 10 s", "○ 10 s"} {
		if !strings.Contains(track, want) {
			t.Errorf("stage track lost %q: %q", want, track)
		}
	}
	if m.run.stages[2].state != stagePending {
		t.Error("an unknown phase moved a stage")
	}
}

func TestMultiServerRunViews(t *testing.T) {
	t.Parallel()
	m := runModel(t, "a", "b")
	withPopulation(m.run.details, "a", goclient.Result{
		Stage:   goclient.StageLatency,
		Latency: goclient.LatencyStats{Count: 3, P50: 10 * time.Millisecond},
	},
		goclient.Result{Stage: goclient.StageDownload, Direction: goclient.Down, MeanBps: 1e6})
	withPopulation(m.run.details, "b", goclient.Result{
		Stage:   goclient.StageLatency,
		Latency: goclient.LatencyStats{Count: 3, P50: 90 * time.Millisecond},
	})
	m.run.results = []goclient.Result{{Stage: goclient.StageDownload, Direction: goclient.Down, MeanBps: 3e6}}
	m, _ = modelAndCmd(m.Update(eventsMsg{
		seq: m.runSeq,
		events: []goclient.Event{
			{
				Kind:     goclient.EventServerFailure,
				ServerID: "b",
				Failure:  &goclient.ServerFailure{ServerID: "b", Err: errors.New("connection lost")},
			},
		},
	}))
	if m.notice != "B: connection lost" {
		t.Fatalf("failure notice = %q, want the server's name", m.notice)
	}
	if screen := view(m); !strings.Contains(screen, "latency to A") || !strings.Contains(screen, "10.0 ms") {
		t.Fatalf("focus A: %q", screen)
	}
	m, _ = modelAndCmd(m.Update(press("l")))
	if screen := view(m); m.run.focus != "b" ||
		!strings.Contains(screen, "latency to B") ||
		!strings.Contains(screen, "90.0 ms") {
		t.Fatalf("focus did not move to B")
	}
	m, _ = modelAndCmd(m.Update(press("d")))
	details := ansi.Strip(view(m))
	combined, perServer := strings.Index(details, "Combined"), strings.Index(details, "│ A ")
	if m.popup != popupDetails || combined < 0 || perServer < combined || !strings.Contains(details, "24.00 Mbit/s") {
		t.Fatalf("details must lead with the result table: %s", details)
	}
	if m, _ = modelAndCmd(m.Update(press("esc"))); m.popup != popupNone {
		t.Fatal("esc did not close details")
	}
}

func TestReadinessRowsAndAvailableServers(t *testing.T) {
	t.Parallel()
	m := testModel(t)
	m.preparedRun = preparedFixture(nil, &goclient.AuthRequiredError{}, errors.New("connection refused"))
	m.cfg.ServerIDs = []string{"a", "b", "c"}
	plan := m.planView(80)
	for _, want := range []string{
		"A",
		"Ready",
		"B",
		"Sign in",
		"C",
		"Unavailable",
		"connection refused",
		"u Use available servers",
	} {
		if !strings.Contains(plan, want) {
			t.Errorf("plan lost %q: %q", want, plan)
		}
	}
	if !m.canUseAvailable() || !strings.Contains(serversRow.row(m).note, "1 of 3 ready") {
		t.Fatalf("available servers not offered: %q", serversRow.row(m).note)
	}
	m, _ = modelAndCmd(m.Update(press("u")))
	if m.notice == "" {
		t.Fatal("use available servers gave no feedback")
	}
}

func TestServerChooserFlow(t *testing.T) {
	t.Parallel()
	m := testModel(t)
	m.prepare = prepareChecking
	if m, _ = modelAndCmd(m.Update(press("s"))); !m.openChooser ||
		!strings.Contains(m.notice, "when the path check finishes") {
		t.Fatalf("deferred chooser notice: %q", m.notice)
	}
	run := preparedFixture(nil)
	run.Catalog = preparedFixture(nil, nil, nil, nil, nil).Catalog
	m, _ = modelAndCmd(m.Update(preparationMsg{seq: m.prepareSeq, run: run}))
	if m.popup != popupServers {
		t.Fatal("the chooser did not open after the check")
	}
	for _, k := range []string{
		"space",
		"down",
		"space",
		"down",
		"space",
		"down",
		"space",
		"down",
		"space",
		"up",
		"up",
		"up",
		"up",
		"space",
	} {
		m, _ = modelAndCmd(m.Update(press(k)))
	}
	if len(m.serverDraft) != 4 || !strings.Contains(m.notice, "At most four") {
		t.Fatalf("draft %v exceeded the limit", m.serverDraft)
	}
	if m, _ = modelAndCmd(m.Update(press("esc"))); m.popup != popupNone || len(m.cfg.ServerIDs) != 1 {
		t.Fatal("esc applied the draft")
	}
}

func TestViewFitsTheTerminal(t *testing.T) {
	t.Parallel()
	for _, size := range [][2]int{{40, 16}, {80, 24}, {160, 50}} {
		width, height := size[0], size[1]
		setup := testModel(t)
		setup.width, setup.height = width, height
		setup.cfg.BaseURL = "https://a-very-long-hostname.internal.example.com:7247"
		setup.preparedRun = preparedFixture(nil, errors.New(strings.Repeat("a long failure ", 8)), nil)
		setup.notice = strings.Repeat("a long notice ", 12)
		run := runModel(t, "a", "b")
		run.width, run.height = width, height
		run.run.results = []goclient.Result{
			{Stage: goclient.StageDownload, Direction: goclient.Down, MeanBps: 1e9, PeakBps: 2e9},
		}
		frames := map[string]string{"run": view(run)}
		for section := range sections {
			setup.section = section
			frames[sections[section].label] = view(setup)
		}
		run.popup = popupDetails
		frames["details"] = view(run)
		setup.beginEdit(catalogueRow, setup.cfg.BaseURL)
		frames["edit"] = view(setup)
		setup.edit = nil
		setup.auth = &signIn{pending: &goclient.PendingAuthorization{Code: "ABCD", BrowserURL: "https://x/" +
			strings.Repeat("a", 200)}}
		frames["sign-in"] = view(setup)
		setup.auth, setup.popup = nil, popupServers
		frames["servers"] = view(setup)
		for name, frame := range frames {
			lines := strings.Split(frame, "\n")
			if len(lines) > height {
				t.Errorf("%s at %dx%d: %d lines", name, width, height, len(lines))
			}
			for i, line := range lines {
				if got := lipgloss.Width(line); got > max(width, 40) {
					t.Errorf("%s at %dx%d: line %d spans %d cells: %q", name, width, height, i, got, line)
				}
			}
		}
	}
}
