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
	t.Cleanup(m.controller.Close)
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
		m.controller.Close()
		for _, p := range []*goclient.Preparation{preparation, m.controller.NewPreparation(m.cfg, nil)} {
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
	t.Cleanup(m.controller.Close)
	pending, err := m.preparation.BeginAuthorization(cfg.BaseURL, cfg.BaseURL+"/login")
	if err != nil {
		t.Fatal(err)
	}
	m.prepare = prepareSignIn
	m, poll := modelAndCmd(m.Update(authChallengeMsg{seq: m.prepareSeq, pending: pending}))
	m, _ = modelAndCmd(m.Update(press("esc")))
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
	t.Cleanup(m.controller.Close)
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
	if done := finishFrom(t, m); done.last != goclient.OutcomeStopped || done.run != nil ||
		!strings.Contains(done.notice, "stopped") {
		t.Fatalf("stopped start reads %q", done.notice)
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
	if m, _ = modelAndCmd(m.Update(press("esc"))); m.run != nil || m.prepare != prepareChecking {
		t.Fatal("esc did not return to a freshly checked setup after the test")
	}
}

func TestQuitDuringARunStopsAndReports(t *testing.T) {
	t.Parallel()
	url, entered, left := hangingServer(t)
	cfg := goclient.DefaultConfig()
	cfg.BaseURL = url
	m := newModel(cfg)
	t.Cleanup(m.controller.Close)
	m, _ = modelAndCmd(m.startRun())
	within(t, entered, "run never reached preparation")
	m, cmd := modelAndCmd(m.Update(press("q")))
	if cmd != nil || !m.quitting {
		t.Fatal("q quit before the running test stopped")
	}
	within(t, left, "q left the run's request alive")
	for !quits(cmd) {
		msg := waitEvents(m.runSeq, m.events)()
		if msg == nil {
			t.Fatal("the run ended without its terminal event")
		}
		m, cmd = modelAndCmd(m.Update(msg))
	}
	if m.running() || m.last != goclient.OutcomeStopped {
		t.Fatalf("quitting before the first server report ended as %q", m.last)
	}
}

func runModel(t *testing.T, servers ...string) model {
	t.Helper()
	m := testModel(t)
	m.cfg.Stages.Bidirectional = true
	m.run = newRunState(m.cfg, "", time.Now())
	details := &goclient.RunDetails{LatencyFocus: servers[0], Participants: servers, Outcome: goclient.OutcomeRunning}
	for _, id := range servers {
		origin := "https://" + id
		details.Servers = append(details.Servers, goclient.ServerRunSummary{
			Server: wire.ServerEntry{ID: id, Name: strings.ToUpper(id), Location: "Somewhere"},
			Throughput: wire.ThroughputTarget{
				Origin: origin, Transport: wire.TransportFetchStream, Protocol: "http2",
			},
			LatencyTarget: &wire.LatencyTarget{Origin: origin, Transport: wire.TransportWebSocket, Protocol: "http1"},
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
		prepareChecking: "Not started",
		prepareSignIn:   "Test cannot start",
		prepareFailed:   "Not started",
		prepareReady:    "Not started",
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
		"Loaded latency · Bidirectional",
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
			TotalBytes: 50_000_000, Elapsed: 10 * time.Second, Samples: 39, Err: context.Canceled,
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
		"40.00 Mbit/s", "receiver-timed", "Upload stopped.", "Bi-dir",
		"Added: loaded median minus idle median.",
	} {
		if !strings.Contains(text, want) {
			t.Errorf("report lost %q:\n%s", want, text)
		}
	}
	m.run.stages[3].state = stageStopped
	if text := m.finalReport(); !strings.Contains(text, "Stopped") || strings.Contains(text, "canceled") {
		t.Errorf("a stage stopped before measuring is not named: %s", text)
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

func TestIdleReadingHoldsTheLastReplyThroughTimeouts(t *testing.T) {
	t.Parallel()
	m := runModel(t, "a")
	probe := func(sample goclient.LatencySample) goclient.Event {
		return goclient.Event{Kind: goclient.EventLatency, ServerID: "a", Latency: sample}
	}
	reply, timeout := probe(goclient.LatencySample{RTT: 12e6}), probe(goclient.LatencySample{TimedOut: true})
	stage := func(phase goclient.Phase) goclient.Event {
		return goclient.Event{Kind: goclient.EventStage, Stage: goclient.StageLatency, Phase: phase}
	}
	for _, c := range []struct {
		name          string
		events        []goclient.Event
		want, without string
	}{
		{"reply", []goclient.Event{stage(goclient.PhaseMeasuring), reply}, "12.0 ms", "probe timeout"},
		{"timeouts hold the reply", []goclient.Event{timeout, timeout}, "12.0 ms  probe timeout ×2", ""},
		{"a reply ends the streak", []goclient.Event{reply}, "12.0 ms", "probe timeout"},
		{"a new stage starts empty", []goclient.Event{stage(goclient.PhasePreparing), stage(goclient.PhaseMeasuring)},
			"Idle latency —", "12.0 ms"},
	} {
		m, _ = modelAndCmd(m.Update(eventsMsg{seq: m.runSeq, events: c.events}))
		live := ansi.Strip(m.liveView(60, 16))
		if !strings.Contains(live, c.want) || c.without != "" && strings.Contains(live, c.without) {
			t.Errorf("%s: live view %q, want %q without %q", c.name, live, c.want, c.without)
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
	unavailable := goclient.Result{Stage: goclient.StageDownload, Direction: goclient.Down, Unavailable: true}
	m.apply(goclient.Event{Kind: goclient.EventResult, Stage: goclient.StageDownload, Result: &unavailable})
	m.apply(goclient.Event{Kind: goclient.EventStage, Stage: goclient.StageDownload, Phase: goclient.PhaseFinished})
	m.run.details.Failures = []goclient.ServerFailure{{ServerID: "a", Stage: goclient.StageUpload}}
	m.apply(goclient.Event{Kind: goclient.EventStage, Stage: goclient.StageUpload, Phase: goclient.PhaseFinished})
	track = ansi.Strip(strings.Join(m.stageTrack(80), "\n"))
	for _, want := range []string{"✓ 4 s", "Download      ! Incomplete", "Upload        ! Partial"} {
		if !strings.Contains(track, want) {
			t.Errorf("stage track lost %q: %q", want, track)
		}
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
		"Sign-in required",
		"C",
		"Failed",
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
	m.prepare = prepareReady
	m.preparedRun.VerifiedAt = time.Now().Add(-goclient.PreparationFreshness - time.Second)
	if plan := m.planView(80); !strings.Contains(plan, "Recheck needed") || strings.Contains(plan, "Ready") {
		t.Fatalf("an expired preparation still reads ready: %q", plan)
	}
}

func TestRunAgainKeepsTheLastResultsUntilTheNextRunStarts(t *testing.T) {
	t.Parallel()
	m := runModel(t, "a")
	m, _ = modelAndCmd(m.Update(eventsMsg{seq: m.runSeq, events: []goclient.Event{
		{Kind: goclient.EventDone, Servers: &goclient.RunDetails{Outcome: goclient.OutcomeComplete}},
	}}))
	previous, refusing := m.run, httptest.NewServer(http.NotFoundHandler())
	defer refusing.Close()
	m.cfg.BaseURL = refusing.URL
	m, _ = modelAndCmd(m.Update(press("r")))
	if m.run != previous || m.statusLabel() != "Checking paths" || !m.running() {
		t.Fatalf("run again replaced the results before the run started: %q", m.statusLabel())
	}
	if m = finishFrom(t, m); m.run != previous || !strings.HasPrefix(m.notice, "Test cannot start:") {
		t.Fatalf("a failed start lost the previous run or its reason: %q", m.notice)
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
	small := testModel(t)
	small.width, small.height = 30, 10
	if !strings.Contains(view(small), "Enlarge the terminal") {
		t.Error("a terminal below the minimum size is not told so")
	}
	for _, size := range [][2]int{{minWidth, minHeight}, {80, 24}, {160, 50}} {
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
			if !strings.Contains(lines[0], "Graphite Meter") ||
				!strings.HasSuffix(strings.TrimSpace(ansi.Strip(lines[len(lines)-1])), "quit") {
				t.Errorf("%s at %dx%d lost its chrome:\n%s", name, width, height, ansi.Strip(frame))
			}
			for _, line := range lines {
				line = strings.TrimSpace(ansi.Strip(line))
				if strings.HasPrefix(line, "│") && !strings.HasSuffix(line, "│") ||
					strings.HasPrefix(line, "╭") && !strings.HasSuffix(line, "╮") {
					t.Errorf("%s at %dx%d: a panel lost its right border: %q", name, width, height, line)
				}
			}
			for i, line := range lines {
				if got := lipgloss.Width(line); got > width {
					t.Errorf("%s at %dx%d: line %d spans %d cells: %q", name, width, height, i, got, line)
				}
			}
		}
	}
}

func TestGrantsFollowTheCatalogueOrigin(t *testing.T) {
	t.Parallel()
	a, b := httptest.NewServer(http.NotFoundHandler()), httptest.NewServer(http.NotFoundHandler())
	defer a.Close()
	defer b.Close()
	m := testModel(t)
	if err := m.controller.AcceptAuthorization(a.URL, "grant"); err != nil {
		t.Fatal(err)
	}
	for _, c := range []struct {
		typed     string
		presented bool
	}{
		{b.URL, false},
		{strings.ToUpper(a.URL) + "/", true},
	} {
		m.beginEdit(catalogueRow, "")
		for _, r := range c.typed {
			m, _ = modelAndCmd(m.Update(press(string(r))))
		}
		m, _ = modelAndCmd(m.Update(press("enter")))
		_, err := m.preparation.PrepareRun()
		// A plain-HTTP origin refuses the grant, which shows whether one was attached.
		if got := err != nil && strings.Contains(err.Error(), "authentication grant"); got != c.presented {
			t.Errorf("catalogue %s: grant attached = %v, want %v (%v)", c.typed, got, c.presented, err)
		}
	}
}

func TestFailedRunShowsNoActivity(t *testing.T) {
	t.Parallel()
	m := testModel(t)
	m.run = newRunState(m.cfg, "", time.Now())
	done := goclient.Event{Kind: goclient.EventDone, Err: errors.New("refused")}
	m, _ = modelAndCmd(m.Update(eventsMsg{seq: m.runSeq, events: []goclient.Event{done}}))
	screen, report := view(m), m.finalReport()
	for _, stale := range []string{"Checking paths", "○", "Skipped", "Median"} {
		if strings.Contains(screen+report, stale) {
			t.Errorf("failed run still shows %q:\n%s\n%s", stale, screen, report)
		}
	}
	if !strings.Contains(screen, "not run") || !strings.HasSuffix(report, "refused") {
		t.Errorf("failed run hides its unrun stages or error:\n%s\n%s", screen, report)
	}
}

func TestScrollingRevealsTheWholeBody(t *testing.T) {
	t.Parallel()
	m := runModel(t, "a", "b")
	m.width, m.height = 80, 24
	m.run.outcome = goclient.OutcomeComplete
	for _, stage := range []goclient.Stage{goclient.StageDownload, goclient.StageUpload, goclient.StageBidirectional} {
		m.run.results = append(m.run.results, goclient.Result{Stage: stage, Direction: goclient.Down, MeanBps: 1e9})
	}
	if f := m.layout(); len(f.body) <= f.bodyH || !strings.Contains(ansi.Strip(f.footer), "pgdn more") {
		t.Fatalf("a %d-line body in %d rows offers no scrolling: %q", len(f.body), f.bodyH, f.footer)
	}
	m.width, m.height = minWidth, minHeight
	f := m.layout()
	if footer := ansi.Strip(f.footer); !strings.Contains(footer, "pgdn more") || !strings.Contains(footer, "quit") {
		t.Fatalf("the narrow key hints hide scrolling: %q", footer)
	}
	if len(f.body) <= f.bodyH {
		t.Fatalf("a %d-line body in %d rows offers no scrolling: %q", len(f.body), f.bodyH, f.footer)
	}
	seen := map[string]bool{}
	for range len(f.body) {
		for _, line := range strings.Split(ansi.Strip(view(m)), "\n") {
			seen[strings.TrimSpace(line)] = true
		}
		m, _ = modelAndCmd(m.Update(press("down")))
	}
	if !m.bodyViewport(m.layout()).AtBottom() {
		t.Fatalf("scrolling stopped at %d of %d lines", m.body.YOffset(), len(f.body))
	}
	for _, line := range f.body {
		if want := strings.TrimSpace(ansi.Strip(line)); !seen[want] {
			t.Errorf("line never shown: %q", want)
		}
	}
}

func TestResetAsksFirst(t *testing.T) {
	t.Parallel()
	m := testModel(t)
	m.cfg.Warmup, m.section, m.row = time.Second, 2, 5
	m, _ = modelAndCmd(m.Update(press("enter")))
	if m.cfg.Warmup != time.Second || !m.resetPrompt {
		t.Fatal("reset did not ask first")
	}
	m, _ = modelAndCmd(m.Update(press("x")))
	if m.cfg.Warmup != time.Second || m.resetPrompt || m.notice != "Settings kept." {
		t.Fatal("another key did not keep the settings")
	}
	m, _ = modelAndCmd(m.Update(press("enter")))
	m, _ = modelAndCmd(m.Update(press("enter")))
	if m.cfg.Warmup != goclient.DefaultConfig().Warmup || m.resetPrompt {
		t.Fatal("confirmed reset kept the settings")
	}
}
