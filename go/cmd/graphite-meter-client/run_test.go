package main

import (
	"context"
	"errors"
	"fmt"
	"image/color"
	"math"
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
	if m, _ = modelAndCmd(m.Update(press("esc"))); !m.stopping {
		t.Fatal("esc did not stop the test")
	}
	within(t, left, "stopping left the run's request alive")
	if done := finishFrom(t, m); done.last != goclient.OutcomeStopped || done.run != nil ||
		!strings.Contains(notice(done), "stopped") {
		t.Fatalf("stopped start reads %q", notice(done))
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
	m.run = newRunState(m.cfg, time.Now())
	for _, k := range []string{"r", "enter", "1", "x"} {
		m, _ = modelAndCmd(m.Update(press(k)))
	}
	if m.runSeq != 0 || !m.cfg.Stages.Latency || m.stopping {
		t.Fatal("a key restarted the running test, switched a stage or stopped it")
	}
	if m, _ = modelAndCmd(m.Update(press("esc"))); !m.stopping {
		t.Fatal("esc did not stop the test at once")
	}
}

func TestQuitDuringARunStopsAndReports(t *testing.T) {
	t.Parallel()
	url, entered, left := hangingServer(t)
	cfg := goclient.DefaultConfig()
	cfg.BaseURL = url
	for _, k := range []string{"q", "ctrl+c"} {
		m := newModel(cfg)
		t.Cleanup(m.controller.Close)
		m, _ = modelAndCmd(m.startRun())
		within(t, entered, "run never reached preparation")
		m, cmd := modelAndCmd(m.Update(press(k)))
		if cmd != nil || !m.quitting {
			t.Fatalf("%s quit before the running test stopped", k)
		}
		within(t, left, k+" left the run's request alive")
		for !quits(cmd) {
			msg := waitEvents(m.runSeq, m.events)()
			if msg == nil {
				t.Fatal("the run ended without its terminal event")
			}
			m, cmd = modelAndCmd(m.Update(msg))
		}
		if m.running() || m.last != goclient.OutcomeStopped || m.interrupted != (k == "ctrl+c") {
			t.Fatalf("%s before the first server report ended as %q", k, m.last)
		}
	}
	m := runModel(t, "a")
	m, _ = modelAndCmd(m.Update(interruptMsg{}))
	if _, cmd := modelAndCmd(m.Update(press("ctrl+c"))); !m.quitting || !quits(cmd) {
		t.Fatal("a second interrupt did not quit at once")
	}
}

func runModel(t *testing.T, servers ...string) model {
	t.Helper()
	m := testModel(t)
	m.cfg.Stages.Bidirectional = true
	m.run = newRunState(m.cfg, time.Now())
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

// statusOf is the state the bottom bar reads.
func statusOf(m model) string {
	label, _ := m.status()
	return label
}

// screen is the frame at w×h without its styles.
func screen(m model, w, h int) string {
	m.width, m.height = w, h
	return ansi.Strip(view(m))
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
		{goclient.Event{Kind: goclient.EventStage, Stage: goclient.StageLatency, Phase: goclient.PhasePreparing},
			"Checking paths"},
		{goclient.Event{Kind: goclient.EventStage, Stage: goclient.StageDownload, Phase: goclient.PhaseWarmup},
			"Warmup"},
		{goclient.Event{Kind: goclient.EventStage, Stage: goclient.StageBidirectional, Phase: goclient.PhaseMeasuring},
			"Bidirectional"},
	} {
		m, _ = modelAndCmd(m.Update(eventsMsg{seq: m.runSeq, events: []goclient.Event{c.event}}))
		if got := statusOf(m); got != c.want {
			t.Errorf("after %v/%v the header reads %q, want %q", c.event.Stage, c.event.Phase, got, c.want)
		}
	}
	for _, c := range []struct {
		done goclient.Event
		want string
	}{
		{goclient.Event{Kind: goclient.EventDone, Servers: &goclient.RunDetails{Outcome: goclient.OutcomePartial}},
			"Partial"},
		{goclient.Event{Kind: goclient.EventDone, Err: fmt.Errorf("stage: %w", context.Canceled)}, "Stopped"},
		{goclient.Event{Kind: goclient.EventDone, Err: errors.New("server said context canceled")}, "Failed"},
		{goclient.Event{Kind: goclient.EventDone, Err: errors.New("lost"),
			Servers: &goclient.RunDetails{Outcome: goclient.OutcomeIncomplete}}, "Incomplete"},
	} {
		finished := runModel(t, "a")
		finished, _ = modelAndCmd(finished.Update(eventsMsg{seq: finished.runSeq, events: []goclient.Event{c.done}}))
		if got := statusOf(finished); got != c.want || strings.Contains(view(finished), "canceling") {
			t.Errorf("outcome %v reads %q, want %q", c.done.Outcome(), got, c.want)
		}
	}
	setup := testModel(t)
	for state, want := range map[prepareState]string{
		prepareChecking: "Not started",
		prepareSignIn:   "Sign in",
		prepareFailed:   "Test could not start",
		prepareReady:    "Not started",
	} {
		setup.prepare = state
		if got := statusOf(setup); got != want {
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
		m.run.running() ||
		!errors.Is(m.run.err, failure) ||
		len(m.run.results) != 1 ||
		m.run.stages[3].state != stageFailed {
		t.Fatalf("terminal state: %+v", m.run)
	}
	console := screen(m, 120, 40)
	for _, want := range []string{"Bidi… Failed", missing} {
		if !strings.Contains(console, want) {
			t.Errorf("view lost %q:\n%s", want, console)
		}
	}
	if report := ansi.Strip(m.finalReport()); !strings.Contains(report, "unfinished probes 2") {
		t.Errorf("the report lost the unfinished probes:\n%s", report)
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
			Count: 2, MeanRawRTT: 10 * time.Millisecond,
		},
	}
	loaded := goclient.LatencyStats{Count: 40, P50: 17800 * time.Microsecond, P95: 30 * time.Millisecond, Timeouts: 2}
	withPopulation(m.run.details, "a",
		goclient.Result{Stage: goclient.StageLatency, Latency: idle},
		goclient.Result{Stage: goclient.StageDownload, Latency: loaded})
	m.run.outcome = goclient.OutcomeComplete
	text := ansi.Strip(m.finalReport())
	for _, want := range []string{
		outcomeLabels[goclient.OutcomeComplete], compactPopulation(goclient.StageLatency),
		populationLabel(goclient.StageDownload), fmtMs(idle.P50), fmtMs(idle.P95), fmtMs(idle.Jitter),
		fmtRate(117_500_000), "peak 1000 ·", fmtBytes(1_200_000_000), fmtMs(loaded.P50),
		fmtAdded(loaded.P50 - idle.P50), fmtRate(5_000_000), stageLabels[goclient.StageBidirectional],
	} {
		if !strings.Contains(text, want) {
			t.Errorf("report lost %q:\n%s", want, text)
		}
	}
	m.run.stages[3].state, m.run.outcome = stageStopped, goclient.OutcomeStopped
	if text = ansi.Strip(m.finalReport()); !strings.Contains(text, "Stopped") || strings.Contains(text, "canceled") {
		t.Errorf("a stage stopped before measuring is not named: %s", text)
	}
	if strings.Contains(strings.ToLower(text), "loss") || !strings.Contains(text, "Upload stopped") ||
		strings.Contains(text, populationLabel(goclient.StageLatency)) {
		t.Errorf("report vocabulary or a population note without an issue: %q", text)
	}
	m.run = nil
	if m.finalReport() != "" {
		t.Error("report before any run")
	}
}

func TestConsoleFollowsTheStage(t *testing.T) {
	t.Parallel()
	m := runModel(t, "a")
	for _, c := range []struct {
		stage goclient.Stage
		want  []string
	}{
		{goclient.StageLatency, []string{"Running", "3.0 ms"}},
		{goclient.StageDownload, []string{"↓ Download", "Running"}},
		{goclient.StageBidirectional, []string{"↕ Bidirectional", "Running"}},
	} {
		m, _ = modelAndCmd(m.Update(eventsMsg{seq: m.runSeq, events: []goclient.Event{
			{Kind: goclient.EventStage, Stage: c.stage, Phase: goclient.PhaseMeasuring, At: time.Now()},
			{Kind: goclient.EventThroughput, Direction: goclient.Down, At: time.Now(),
				Throughput: goclient.ThroughputSample{BytesPerSec: 1e6}},
			{Kind: goclient.EventLatency, ServerID: "a", At: time.Now(), Latency: goclient.LatencySample{RTT: 3e6}},
		}}))
		console := screen(m, 120, 36)
		lines := strings.Split(console, "\n")
		if bar := strings.TrimSpace(lines[len(lines)-1]); !strings.HasPrefix(bar, stageLabels[c.stage]) {
			t.Errorf("%s: the bar reads %q", c.stage, bar)
		}
		for _, want := range c.want {
			if !strings.Contains(console, want) {
				t.Errorf("%s console lost %q:\n%s", c.stage, want, console)
			}
		}
		if !strings.ContainsFunc(console, func(r rune) bool { return r > 0x2800 && r <= 0x28ff }) {
			t.Errorf("%s console drew no dial:\n%s", c.stage, console)
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
	measuring := goclient.Event{Kind: goclient.EventStage, Stage: goclient.StageLatency, Phase: goclient.PhaseMeasuring}
	for _, c := range []struct {
		name          string
		events        []goclient.Event
		want, without string
	}{
		{"reply", []goclient.Event{measuring, reply}, "12.0 ms", "unanswered"},
		{"timeouts hold the reply", []goclient.Event{timeout, timeout}, "2 probes unanswered", ""},
		{"a reply ends the streak", []goclient.Event{reply}, "12.0 ms", "unanswered"},
	} {
		m, _ = modelAndCmd(m.Update(eventsMsg{seq: m.runSeq, events: c.events}))
		console := screen(m, 120, 36)
		if !strings.Contains(console, c.want) || c.without != "" && strings.Contains(console, c.without) {
			t.Errorf("%s: console\n%s\nwant %q without %q", c.name, console, c.want, c.without)
		}
	}
	if !strings.Contains(screen(m, 120, 36), "12.0 ms") {
		t.Error("timeouts lost the median")
	}
}

func TestChipsFollowStageEvents(t *testing.T) {
	t.Parallel()
	m := runModel(t, "a")
	start := time.Now()
	m.now = start.Add(2500 * time.Millisecond)
	stage := func(stage goclient.Stage, phase goclient.Phase) goclient.Event {
		return goclient.Event{Kind: goclient.EventStage, Stage: stage, Phase: phase, At: start}
	}
	withPopulation(m.run.details, "a", goclient.Result{Stage: goclient.StageLatency, Latency: goclient.LatencyStats{
		Count: 3, P50: 4 * time.Millisecond}})
	m.apply(stage(goclient.StageLatency, goclient.PhaseFinished))
	m.apply(stage(goclient.StageDownload, goclient.PhaseMeasuring))
	m.apply(stage(goclient.StageUpload, goclient.Phase(99)))
	chips := func() string {
		for line := range strings.SplitSeq(screen(m, 120, 36), "\n") {
			if strings.Contains(line, "≈ Lat") && strings.Contains(line, "↑ Up") {
				return line
			}
		}
		return ""
	}
	for _, want := range []string{"✓", "2.5 s", "10 s"} {
		if !strings.Contains(chips(), want) {
			t.Errorf("the chips lost %q: %q", want, chips())
		}
	}
	if m.run.stages[2].state != stagePending {
		t.Error("an unknown phase moved a stage")
	}
	unavailable := goclient.Result{Stage: goclient.StageDownload, Direction: goclient.Down, Unavailable: true}
	m.apply(goclient.Event{Kind: goclient.EventResult, Stage: goclient.StageDownload, Result: &unavailable})
	m.apply(stage(goclient.StageDownload, goclient.PhaseFinished))
	m.run.details.Failures = []goclient.ServerFailure{{ServerID: "a", Stage: goclient.StageUpload}}
	m.apply(stage(goclient.StageUpload, goclient.PhaseFinished))
	for _, want := range []string{"Failed", "Partial", "✓"} {
		if !strings.Contains(chips(), want) {
			t.Errorf("the chips lost %q: %q", want, chips())
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
				Failure: &goclient.ServerFailure{ServerID: "b", Reason: goclient.FailureConnectionLost,
					Err: errors.New(`Get "http://b/download": EOF`)},
			},
		},
	}))
	if notice(m) != "B: Connection lost" {
		t.Fatalf("failure notice = %q, want the server's name and reason", notice(m))
	}
	m.run.outcome = goclient.OutcomeComplete
	if console := screen(m, 120, 40); !strings.Contains(console, "to A  l switches") || !strings.Contains(console, "10.0 ms") {
		t.Fatalf("focus A:\n%s", console)
	}
	m, _ = modelAndCmd(m.Update(press("l")))
	if console := screen(m, 120, 40); m.run.latencyServer() != "b" ||
		!strings.Contains(console, "to B  l switches") ||
		!strings.Contains(console, "90.0 ms") {
		t.Fatalf("focus did not move to B:\n%s", console)
	}
	if report := ansi.Strip(m.finalReport()); !strings.Contains(report, "Latency to A") ||
		!strings.Contains(report, "10.0 ms") {
		t.Fatalf("the report followed the viewer's pick instead of the run's focus: %s", report)
	}
	if console := screen(m, 120, 40); !strings.Contains(console, "24.0") {
		t.Fatalf("the download card lost the servers' sum:\n%s", console)
	}
	m.preparedRun = preparedFixture(nil, nil)
	m, _ = modelAndCmd(m.Update(press("d")))
	if details := screen(m, 120, 40); m.overlay != overlayDetails || !strings.Contains(details, "8.00 Mbit/s") ||
		!strings.Contains(details, "Latency to B") {
		t.Fatalf("details lost a server's rate or the latency server:\n%s", details)
	}
	if m, _ = modelAndCmd(m.Update(press("esc"))); m.overlay != overlayNone {
		t.Fatal("esc did not close details")
	}
}

func TestLatencyFollowsTheRunFocus(t *testing.T) {
	t.Parallel()
	m := runModel(t, "a", "b")
	publish := func(focus string, participants ...string) {
		details := *m.run.details
		details.LatencyFocus, details.Participants = focus, participants
		m, _ = modelAndCmd(m.Update(eventsMsg{seq: m.runSeq, events: []goclient.Event{
			{Kind: goclient.EventServers, Servers: &details},
		}}))
	}
	m, _ = modelAndCmd(m.Update(press("l")))
	for _, c := range []struct {
		focus        string
		participants []string
		want         string
	}{
		{"a", []string{"a", "b"}, "b"},
		{"a", []string{"a"}, "a"},
		{"b", []string{"b"}, "b"},
		{"", nil, "b"},
	} {
		if publish(c.focus, c.participants...); m.run.latencyServer() != c.want {
			t.Errorf("focus %q with %v shows %q, want %q", c.focus, c.participants, m.run.latencyServer(), c.want)
		}
	}
}

func TestReadinessNamesEveryServer(t *testing.T) {
	t.Parallel()
	m := testModel(t)
	m.preparedRun = preparedFixture(nil, &goclient.AuthRequiredError{}, errors.New("connection refused"))
	m.cfg.ServerIDs = []string{"a", "b", "c"}
	m.prepare = prepareFailed
	if !strings.Contains(serversRow.row(m).value, "1 of 3 ready") {
		t.Fatalf("the servers row reads %q", serversRow.row(m).value)
	}
	if bar := strings.Split(screen(m, 120, 40), "\n")[0]; !strings.Contains(bar, "A, B, C") {
		t.Fatalf("the top bar lost a server: %q", bar)
	}
	m.overlay = overlayChooser
	m.serverDraft = m.cfg.ServerIDs
	if chooser := screen(m, 120, 40); !strings.Contains(chooser, "Ready") || !strings.Contains(chooser, "Sign in") ||
		!strings.Contains(chooser, "Failed") {
		t.Errorf("the chooser lost a server's readiness:\n%s", chooser)
	}
	if m.overlay = overlayDetails; !strings.Contains(screen(m, 120, 40), "connection refused") {
		t.Errorf("details lost why a server failed:\n%s", screen(m, 120, 40))
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
	if m.run != previous || statusOf(m) != "Checking paths" || !m.running() {
		t.Fatalf("run again replaced the results before the run started: %q", statusOf(m))
	}
	if m = finishFrom(t, m); m.run != previous || !strings.HasPrefix(notice(m), "Test could not start:") {
		t.Fatalf("a failed start lost the previous run or its reason: %q", notice(m))
	}
}

func TestServerChooserFlow(t *testing.T) {
	t.Parallel()
	m := testModel(t)
	m.prepare = prepareChecking
	m = m.openSettings(serversRow)
	if m, _ = modelAndCmd(m.Update(press("enter"))); !m.openChooser ||
		!strings.Contains(notice(m), "once the check finishes") {
		t.Fatalf("deferred chooser notice: %q", notice(m))
	}
	run := preparedFixture(nil)
	run.Catalog = preparedFixture(nil, nil, nil, nil, nil).Catalog
	m, _ = modelAndCmd(m.Update(preparationMsg{seq: m.prepareSeq, run: run}))
	if m.overlay != overlayChooser {
		t.Fatal("the chooser did not open after the check")
	}
	for _, k := range strings.Fields("space down space down space down space down space up up up up space") {
		m, _ = modelAndCmd(m.Update(press(k)))
	}
	if len(m.serverDraft) != 4 || !strings.Contains(notice(m), "At most four") {
		t.Fatalf("draft %v exceeded the limit", m.serverDraft)
	}
	if m, _ = modelAndCmd(m.Update(press("esc"))); m.overlay == overlayChooser || len(m.cfg.ServerIDs) != 1 {
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
	for _, size := range [][2]int{smallest, {80, 24}, {160, 50}} {
		width, height := size[0], size[1]
		setup := testModel(t)
		setup.width, setup.height = width, height
		setup.cfg.BaseURL = "https://a-very-long-hostname.internal.example.com:7247"
		setup.preparedRun = preparedFixture(nil, errors.New(strings.Repeat("a long failure ", 8)), nil)
		setup = setup.notify(styled(strings.Repeat("a long notice ", 12), setup.pal.warn))
		run := runModel(t, "a", "b")
		run.width, run.height = width, height
		run.run.results = []goclient.Result{
			{Stage: goclient.StageDownload, Direction: goclient.Down, MeanBps: 1e9, PeakBps: 2e9},
		}
		frames := map[string]model{"run": run, "console": setup}
		settings := setup.openSettings(resetRow)
		frames["settings"] = settings
		run.overlay = overlayDetails
		frames["details"] = run
		run.overlay = overlayHelp
		frames["keys"] = run
		frames["connect"] = setup.askForServer()
		settings.beginEdit(catalogueRow, setup.cfg.BaseURL)
		frames["edit"] = settings
		signing := setup
		signing.auth = &signIn{pending: &goclient.PendingAuthorization{Code: "ABCD", BrowserURL: "https://x/" +
			strings.Repeat("a", 200)}, since: time.Now()}
		frames["sign-in"] = signing
		setup.overlay = overlayChooser
		frames["servers"] = setup
		for name, fm := range frames {
			lines := strings.Split(view(fm), "\n")
			if len(lines) != height {
				t.Errorf("%s at %dx%d: %d lines", name, width, height, len(lines))
			}
			if bar := strings.TrimSpace(ansi.Strip(lines[len(lines)-1])); !strings.Contains(lines[0], "Graphite Meter") ||
				!strings.HasPrefix(bar, statusOf(fm)) {
				t.Errorf("%s at %dx%d lost its bars:\n%s", name, width, height, ansi.Strip(view(fm)))
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

func TestEventsBeforeTheServersLeaveThePreviousRunAlone(t *testing.T) {
	t.Parallel()
	m := runModel(t, "a")
	previous := m.run
	m.next = newRunState(m.cfg, time.Now())
	stage := goclient.Event{Kind: goclient.EventStage, Stage: goclient.StageDownload, Phase: goclient.PhaseMeasuring}
	m, _ = modelAndCmd(m.Update(eventsMsg{seq: m.runSeq, events: []goclient.Event{stage}}))
	if m.run != previous || m.next == nil || previous.stage != "" {
		t.Fatalf("an event before EventServers reached the previous run: %+v", m.run)
	}
}

func TestFailedRunShowsNoActivity(t *testing.T) {
	t.Parallel()
	m := testModel(t)
	m.run = newRunState(m.cfg, time.Now())
	done := goclient.Event{Kind: goclient.EventDone, Err: errors.New("refused")}
	m, _ = modelAndCmd(m.Update(eventsMsg{seq: m.runSeq, events: []goclient.Event{done}}))
	console, report := screen(m, 120, 40), ansi.Strip(m.finalReport())
	for _, stale := range []string{"Checking paths", "Running", "Warming up"} {
		if strings.Contains(console+report, stale) {
			t.Errorf("failed run still shows %q:\n%s\n%s", stale, console, report)
		}
	}
	if !strings.Contains(console, "refused") || !strings.HasSuffix(report, "refused") {
		t.Errorf("failed run hides its error:\n%s\n%s", console, report)
	}
}

func TestScrollingRevealsTheWholeSheet(t *testing.T) {
	t.Parallel()
	m := runModel(t, "a", "b")
	m.preparedRun = preparedFixture(nil, nil, nil)
	m.width, m.height = smallest[0], smallest[1]
	m.overlay = overlayDetails
	var want []string
	for _, e := range m.detailsEntries() {
		if e.heading != "" {
			want = append(want, e.heading)
		}
	}
	seen := map[string]bool{}
	for range 40 {
		for _, line := range strings.Split(ansi.Strip(view(m)), "\n") {
			seen[strings.TrimSpace(line)] = true
		}
		m, _ = modelAndCmd(m.Update(press("down")))
	}
	if m.frame.scroll == 0 || m.frame.scroll != m.frame.maxScroll {
		t.Fatalf("scrolling stopped at %d of %d", m.frame.scroll, m.frame.maxScroll)
	}
	for _, heading := range want {
		if !seen[heading] {
			t.Errorf("heading never shown: %q", heading)
		}
	}
	m, _ = modelAndCmd(m.Update(press("home")))
	if view(m); m.frame.scroll != 0 {
		t.Fatalf("home left the sheet at %d", m.frame.scroll)
	}
}

func TestResetAsksFirst(t *testing.T) {
	t.Parallel()
	m := testModel(t)
	m.cfg.Warmup = time.Second
	m = m.openSettings(resetRow)
	m, _ = modelAndCmd(m.Update(press("enter")))
	if m.cfg.Warmup != time.Second || !m.resetPrompt {
		t.Fatal("reset did not ask first")
	}
	m, _ = modelAndCmd(m.Update(press("up")))
	if m.cfg.Warmup != time.Second || m.resetPrompt {
		t.Fatal("moving away did not keep the settings")
	}
	m, _ = modelAndCmd(m.Update(press("down")))
	m, _ = modelAndCmd(m.Update(press("enter")))
	m, _ = modelAndCmd(m.Update(press("enter")))
	if m.cfg.Warmup != goclient.DefaultConfig().Warmup || m.resetPrompt {
		t.Fatal("confirmed reset kept the settings")
	}
}

func TestLineJoinsSamplesAndBreaksOnlyAtGaps(t *testing.T) {
	t.Parallel()
	var tr trace
	for i := range 200 {
		v := 1e6 + 9e5*math.Sin(float64(i)/3)
		switch {
		case i == 100:
			v = math.NaN()
		case i > 100 && i < 130:
			continue
		}
		tr = tr.add(float64(i)*0.1, v)
	}
	for _, w := range []int{25, 65, 105} {
		c := newCanvas(w, 10, true)
		traceLine(c, c.area(), tr.points, nil, 2e6, 0, 20, color.White, color.White, nil)
		inked := map[int]bool{}
		for y := range 10 {
			for x := range w {
				if bits := []rune(c.cell(x, y).Content); len(bits) == 1 && bits[0] > 0x2800 && bits[0] <= 0x28ff {
					dots := int(bits[0]) - 0x2800
					inked[2*x] = inked[2*x] || dots&0x47 != 0
					inked[2*x+1] = inked[2*x+1] || dots&0xb8 != 0
				}
			}
		}
		dot := func(t float64) int { return int(t / 20 * float64(2*w)) }
		for x := range dot(19.9) + 1 {
			if gap := x >= dot(10) && x < dot(13); inked[x] == gap {
				t.Errorf("width %d: dot column %d inked=%v, gap %d–%d", w, x, inked[x], dot(10), dot(13))
			}
		}
	}
}

func TestLiveRatesWaitForEvidence(t *testing.T) {
	t.Parallel()
	m := runModel(t, "a")
	stage := goclient.Event{Kind: goclient.EventStage, At: m.run.started, Stage: goclient.StageDownload,
		Phase: goclient.PhaseMeasuring}
	m, _ = modelAndCmd(m.Update(eventsMsg{seq: m.runSeq, events: []goclient.Event{stage}}))
	shown := func() bool { return strings.Contains(screen(m, 120, 36), "8.00 Mbit/s") }
	if shown() {
		t.Fatal("a rate before the first sample")
	}
	// The first window only anchors the rate; it shows once half a second of evidence follows.
	at := stage.At
	sample := func() {
		at = at.Add(250 * time.Millisecond)
		e := goclient.Event{Kind: goclient.EventThroughput, At: at, Direction: goclient.Down,
			Throughput: goclient.ThroughputSample{BytesPerSec: 1e6}}
		m, _ = modelAndCmd(m.Update(eventsMsg{seq: m.runSeq, events: []goclient.Event{e}}))
	}
	for range 2 {
		sample()
	}
	if shown() {
		t.Fatal("a rate before half a second of evidence")
	}
	sample()
	if !shown() {
		t.Fatalf("half a second of evidence did not show as measured:\n%s", screen(m, 120, 36))
	}
}

func TestFinishedConsoleFitsTheTerminal(t *testing.T) {
	t.Parallel()
	m := runModel(t, "a")
	m.run.outcome = goclient.OutcomeComplete
	download := goclient.Result{Stage: goclient.StageDownload, Direction: goclient.Down, MeanBps: 2e9 / 8}
	m.apply(goclient.Event{Kind: goclient.EventResult, Stage: goclient.StageDownload, Result: &download})
	withPopulation(m.run.details, "a", goclient.Result{Stage: goclient.StageLatency,
		Latency: goclient.LatencyStats{Count: 3, P50: time.Millisecond}})
	for _, size := range [][2]int{{80, 24}, {100, 30}, {120, 40}} {
		m.width, m.height = size[0], size[1]
		screen := ansi.Strip(view(m))
		if !strings.Contains(screen, "Run again") || !strings.Contains(screen, "2.00 Gbit/s") {
			t.Errorf("%dx%d finished screen:\n%s", size[0], size[1], screen)
		}
		for _, line := range strings.Split(screen, "\n") {
			if lipgloss.Width(line) > size[0] {
				t.Errorf("%dx%d: %q overflows", size[0], size[1], line)
			}
		}
	}
}

func TestMissingEvidenceIsNeverShownAsMeasured(t *testing.T) {
	t.Parallel()
	m := runModel(t, "a")
	m.cfg.Stages = goclient.StageSet{Latency: true, Upload: true}
	m.run.plan = m.cfg.Plan()
	m.run.outcome = goclient.OutcomeIncomplete
	m.run.results = []goclient.Result{{Stage: goclient.StageUpload, Direction: goclient.Up, Unavailable: true}}
	m.run.details.Outcome = goclient.OutcomeIncomplete
	m.run.details.Intervals = []goclient.AggregationInterval{{Stage: goclient.StageUpload}}
	withPopulation(m.run.details, "a", goclient.Result{Stage: goclient.StageLatency,
		Latency: goclient.LatencyStats{Count: 3, P50: time.Millisecond}})
	details, report := ansi.Strip(m.detailsView(120, true)), ansi.Strip(m.finalReport())
	for _, wrong := range []string{"0 B", "·  ·", "Added"} {
		if strings.Contains(details+report, wrong) {
			t.Errorf("unmeasured evidence reads %q:\n%s\n%s", wrong, details, report)
		}
	}
	if console := screen(m, 120, 40); strings.Contains(console, "bit/s") {
		t.Errorf("an unmeasured upload reads as a rate:\n%s", console)
	}
}

func TestNarrowConsolesKeepEveryLineOnScreen(t *testing.T) {
	t.Parallel()
	m := runModel(t, "a")
	m.cfg.BidirectionalDuration = 2 * time.Minute
	m.run.outcome = goclient.OutcomeFailed
	for i := range m.run.stages {
		m.run.stages[i].state = []stageState{stageFailed, stagePartial, stageStopped, stageFailed}[i%4]
	}
	for _, width := range []int{smallest[0], 64, 72, 80} {
		console := screen(m, width, 24)
		for _, line := range strings.Split(console, "\n") {
			if lipgloss.Width(line) > width {
				t.Fatalf("%d columns: %q is %d wide", width, line, lipgloss.Width(line))
			}
		}
		if !strings.Contains(console, "Run again") {
			t.Errorf("%d columns lost the run key:\n%s", width, console)
		}
	}
}
