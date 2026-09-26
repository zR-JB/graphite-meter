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

	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
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
			m.beginEdit(rowCatalogue, m.cfg.BaseURL)
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
	m.run = newRunState(m.cfg, "")
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
	m.run = newRunState(m.cfg, "")
	details := &goclient.RunDetails{LatencyFocus: servers[0], Participants: servers, Outcome: goclient.OutcomeRunning}
	for _, id := range servers {
		details.Servers = append(details.Servers, goclient.ServerRunSummary{
			Server:        wire.ServerEntry{ID: id, Name: strings.ToUpper(id), Location: "Somewhere"},
			Throughput:    wire.ThroughputTarget{Transport: wire.TransportFetchStream, Protocol: "http2", TLS: true},
			LatencyTarget: &wire.LatencyTarget{Transport: wire.TransportWebSocket, Protocol: "http1", TLS: true},
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
		if got := finished.statusLabel(); got != c.want || strings.Contains(finished.View(), "canceling") {
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
	view := m.View()
	for _, want := range []string{
		"Incomplete: transfer failed",
		"Loaded latency · Bi-dir",
		"unfinished probes 2",
		"Bi-dir ↓",
		missing,
	} {
		if !strings.Contains(view, want) {
			t.Errorf("view lost %q", want)
		}
	}
}

func TestResultsNameEveryPopulation(t *testing.T) {
	t.Parallel()
	m := runModel(t, "a")
	m.run.results = []goclient.Result{
		{
			Stage:      goclient.StageDownload,
			Direction:  goclient.Down,
			MeanBps:    117_500_000,
			PeakBps:    125_000_000,
			TotalBytes: 1_200_000_000,
			Elapsed:    10 * time.Second,
			Samples:    38,
		},
		{
			Stage:      goclient.StageUpload,
			Direction:  goclient.Up,
			MeanBps:    5_000_000,
			TotalBytes: 50_000_000,
			Elapsed:    10 * time.Second,
			Samples:    39,
		},
	}
	idle := goclient.LatencyStats{
		Count: 16, P50: 10 * time.Millisecond, P95: 12 * time.Millisecond, JitterPairs: 15,
		Jitter: 400 * time.Microsecond, Elapsed: 4 * time.Second,
		ReflectorTiming: &goclient.ReflectorTimingStats{
			Count: 2, MeanRawRTT: 10 * time.Millisecond, MeanAdjustedRTT: 10 * time.Millisecond,
		},
	}
	withPopulation(m.run.details, "a",
		goclient.Result{Stage: goclient.StageLatency, Latency: idle},
		goclient.Result{
			Stage: goclient.StageDownload,
			Latency: goclient.LatencyStats{
				Count:    40,
				P50:      17800 * time.Microsecond,
				P95:      30 * time.Millisecond,
				Timeouts: 2,
			},
		})
	m.run.outcome = goclient.OutcomeComplete
	text := m.finalReport()
	for _, want := range []string{
		"Graphite Meter · Complete",
		"Idle latency", "median 10.0 ms", "p95 12.0 ms", "jitter 0.4 ms", "16 replies", "4.0 s",
		"Server timing (2 paired replies, means): raw 10.0 ms · handling 0.0 ms · adjusted 10.0 ms",
		"Download", "940.0 Mbit/s", "peak 1000 Mbit/s", "1.2 GB", "10.0 s", "38 samples",
		"Loaded latency · Download", "median 17.8 ms", "+7.8 ms added", "probe timeouts 2/42 (4.8%)",
		"Upload", "40.00 Mbit/s", "receiver-timed",
	} {
		if !strings.Contains(text, want) {
			t.Errorf("report lost %q:\n%s", want, text)
		}
	}
	if strings.Contains(text, "\x1b") ||
		strings.Contains(text, "server-clock") ||
		strings.Contains(strings.ToLower(text), "loss") {
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
		{goclient.StageLatency, []string{"Idle latency", "3.0 ms"}, []string{"Download", "Upload"}},
		{goclient.StageDownload, []string{"Download", "Loaded latency"}, []string{"Upload"}},
		{goclient.StageBidirectional, []string{"Download", "Upload", "Loaded latency"}, nil},
	} {
		m, _ = modelAndCmd(m.Update(eventsMsg{
			seq:    m.runSeq,
			events: []goclient.Event{{Kind: goclient.EventStage, Stage: c.stage, Phase: goclient.PhaseMeasuring}},
		}))
		live := m.liveView(60)
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
	track := strings.Join(m.stageTrack(80), "\n")
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
				Failure:  &goclient.ServerFailure{ServerID: "b", Message: "connection lost"},
			},
		},
	}))
	if m.notice != "B: connection lost" {
		t.Fatalf("failure notice = %q, want the server's name", m.notice)
	}
	if view := m.View(); !strings.Contains(view, "latency to A") || !strings.Contains(view, "10.0 ms") {
		t.Fatalf("focus A: %q", view)
	}
	m, _ = modelAndCmd(m.Update(press("l")))
	if view := m.View(); m.run.focus != "b" ||
		!strings.Contains(view, "latency to B") ||
		!strings.Contains(view, "90.0 ms") {
		t.Fatalf("focus did not move to B")
	}
	m, _ = modelAndCmd(m.Update(press("d")))
	details := m.detailsView(120)
	combined, perServer := strings.Index(details, "Combined"), strings.Index(details, "\nA ")
	if !m.detailsOpen || combined < 0 || perServer < combined || !strings.Contains(details, "24.00 Mbit/s") {
		t.Fatalf("details must lead with the result table: %q", details)
	}
	if m, _ = modelAndCmd(m.Update(press("esc"))); m.detailsOpen {
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
	if !m.canUseAvailable() || !strings.Contains(m.setupRow(rowServers).note, "1 of 3 ready") {
		t.Fatalf("available servers not offered: %q", m.setupRow(rowServers).note)
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
	if !m.serverChooser {
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
	if m, _ = modelAndCmd(m.Update(press("esc"))); m.serverChooser || len(m.cfg.ServerIDs) != 1 {
		t.Fatal("esc applied the draft")
	}
}

func TestViewNeverExceedsTheTerminalWidth(t *testing.T) {
	t.Parallel()
	for _, width := range []int{40, 60, 80, 100, 120, 160} {
		setup := testModel(t)
		setup.width = width
		setup.cfg.BaseURL = "https://a-very-long-hostname.internal.example.com:7247"
		setup.preparedRun = preparedFixture(nil, errors.New(strings.Repeat("a long failure ", 8)))
		setup.notice = strings.Repeat("a long notice ", 12)
		run := runModel(t, "a", "b")
		run.width = width
		run.run.results = []goclient.Result{
			{Stage: goclient.StageDownload, Direction: goclient.Down, MeanBps: 1e9, PeakBps: 2e9},
		}
		frames := map[string]string{"run": run.View()}
		for section := range sections {
			setup.section = section
			frames[sections[section].label] = setup.View()
		}
		for name, frame := range frames {
			for i, line := range strings.Split(frame, "\n") {
				if got := lipgloss.Width(line); got > max(width, 44) {
					t.Errorf("%s at width %d: line %d spans %d cells: %q", name, width, i, got, line)
				}
			}
		}
	}
}
