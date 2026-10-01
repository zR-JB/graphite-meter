package main

import (
	"context"
	"io"
	"net"
	"net/http"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"
	"github.com/zR-JB/graphite-meter/go/internal/config"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
	"github.com/zR-JB/graphite-meter/go/internal/route"
	"github.com/zR-JB/graphite-meter/go/internal/server"
	"github.com/zR-JB/graphite-meter/go/internal/testkit"
)

type tuiSnapshot struct {
	content                  string
	ready, finished, editing bool
	popup                    popup
	offset, seq              int
	phase                    goclient.Phase
	outcome                  goclient.Outcome
}

type observedTUI struct {
	model
	frames chan tuiSnapshot
}

func (m observedTUI) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	next, cmd := m.model.Update(msg)
	m.model = next.(model)
	return m, cmd
}

func (m observedTUI) View() tea.View {
	v := m.model.View()
	f := tuiSnapshot{content: ansi.Strip(v.Content), ready: m.prepare == prepareReady,
		finished: m.finished(), editing: m.edit != nil, popup: m.popup, offset: m.body.YOffset(), seq: m.runSeq}
	if m.run != nil {
		f.phase, f.outcome = m.run.phase, m.run.outcome
	}
	select {
	case m.frames <- f:
	default:
		select {
		case <-m.frames:
		default:
		}
		m.frames <- f
	}
	return v
}

type terminalBytes struct{ n atomic.Int64 }

func (w *terminalBytes) Write(p []byte) (int, error) { w.n.Add(int64(len(p))); return len(p), nil }

func TestTUIEndToEnd(t *testing.T) {
	ctx, cancel := context.WithTimeout(t.Context(), 25*time.Second)
	defer cancel()
	socket, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	address := socket.Addr().String()
	socket.Close()
	scfg := config.Default()
	scfg.Native.H1 = address
	served := make(chan error, 1)
	go func() { served <- server.Run(ctx, &scfg) }()
	t.Cleanup(func() {
		cancel()
		if err := <-served; err != nil {
			t.Error(err)
		}
	})
	client := &http.Client{Timeout: 200 * time.Millisecond}
	origin := "http://" + address
	testkit.Eventually(t, 3*time.Second, "server did not start", func() bool {
		res, err := client.Get(origin + route.Servers)
		if err != nil {
			return false
		}
		res.Body.Close()
		return res.StatusCode == http.StatusOK
	})
	cfg := goclient.DefaultConfig()
	cfg.BaseURL, cfg.Warmup = origin, 0
	cfg.Stages.Bidirectional = true
	cfg.LatencyDuration, cfg.DownloadDuration, cfg.UploadDuration, cfg.BidirectionalDuration = time.Second, time.Second, time.Second, time.Second
	cfg.PingInterval, cfg.LoadedPingInterval = goclient.PingFast, goclient.PingFast
	cfg.TransferStreams.Forced = 1
	initial := newModel(cfg)
	defer initial.controller.Close()
	input, keys := io.Pipe()
	defer keys.Close()
	frames := make(chan tuiSnapshot, 1)
	output := &terminalBytes{}
	program := tea.NewProgram(observedTUI{model: initial, frames: frames}, tea.WithContext(ctx),
		tea.WithInput(input), tea.WithOutput(output), tea.WithWindowSize(120, 40), tea.WithFPS(fps),
		tea.WithEnvironment([]string{"TERM=xterm-256color"}), tea.WithoutSignalHandler(), tea.WithoutCatchPanics())
	type ended struct {
		model tea.Model
		err   error
	}
	done := make(chan ended, 1)
	go func() { m, err := program.Run(); done <- ended{m, err} }()
	defer program.Kill()
	await := func(what string, match func(tuiSnapshot) bool) tuiSnapshot {
		t.Helper()
		deadline := time.NewTimer(10 * time.Second)
		defer deadline.Stop()
		var last tuiSnapshot
		for {
			select {
			case last = <-frames:
				if match(last) {
					return last
				}
			case <-deadline.C:
				t.Fatalf("%s: last screen:\n%s", what, last.content)
			case end := <-done:
				t.Fatalf("program exited during %s: %v", what, end.err)
			}
		}
	}
	send := func(text string) {
		t.Helper()
		if _, err := io.WriteString(keys, text); err != nil {
			t.Fatal(err)
		}
	}
	await("ready", func(f tuiSnapshot) bool { return f.ready && strings.Contains(f.content, "ready") })
	send("\x1b[B\r")
	await("edit", func(f tuiSnapshot) bool { return f.editing })
	send(strings.Repeat("x", 80) + "TAIL")
	edited := await("long edit", func(f tuiSnapshot) bool { return f.editing && strings.Contains(f.content, "TAIL") })
	for _, line := range strings.Split(edited.content, "\n") {
		if lipgloss.Width(line) > 120 {
			t.Fatal("edit overflowed terminal")
		}
	}
	send("\x01")
	await("edit start", func(f tuiSnapshot) bool {
		for line := range strings.SplitSeq(f.content, "\n") {
			if f.editing && strings.Contains(line, "Catalogue URL") && strings.Contains(line, origin) {
				return true
			}
		}
		return false
	})
	send("\x05")
	await("edit end", func(f tuiSnapshot) bool { return f.editing && strings.Contains(f.content, "TAIL") })
	program.Send(tea.WindowSizeMsg{Width: 40, Height: 12})
	await("narrow edit", func(f tuiSnapshot) bool {
		return f.editing && strings.Contains(f.content, "TAIL") && lipgloss.Width(strings.SplitN(f.content, "\n", 2)[0]) == 40
	})
	program.Send(tea.WindowSizeMsg{Width: 120, Height: 40})
	send("\x1b")
	await("cancel edit", func(f tuiSnapshot) bool { return !f.editing })
	send("r")
	await("measuring", func(f tuiSnapshot) bool { return f.phase == goclient.PhaseMeasuring })
	live := await("download readings", func(f tuiSnapshot) bool {
		return f.phase == goclient.PhaseMeasuring && strings.Contains(f.content, "Timeline · Download")
	})
	combined := false
	for line := range strings.SplitSeq(live.content, "\n") {
		combined = combined || strings.Contains(line, "↓ ") && strings.Contains(line, "Loaded latency")
	}
	if !combined {
		t.Fatalf("live readings wrapped despite fitting on one row:\n%s", live.content)
	}
	program.Send(tea.WindowSizeMsg{Width: 80, Height: 24})
	complete := await("complete", func(f tuiSnapshot) bool { return f.finished && f.outcome == goclient.OutcomeComplete })
	for _, word := range []string{"Results", "Timeline", "Median", "Download", "Upload", "↓ solid · ↑ dashed"} {
		if !strings.Contains(complete.content, word) {
			t.Fatalf("completed screen lost %s:\n%s", word, complete.content)
		}
	}
	program.Send(tea.WindowSizeMsg{Width: 40, Height: 12})
	send("d")
	await("details", func(f tuiSnapshot) bool { return f.popup == popupDetails })
	program.Send(tea.MouseWheelMsg{Button: tea.MouseWheelDown})
	await("wheel scroll", func(f tuiSnapshot) bool { return f.popup == popupDetails && f.offset > 0 })
	send("\x1b[6~")
	await("scroll", func(f tuiSnapshot) bool { return f.popup == popupDetails && f.offset > 0 })
	program.Send(tea.WindowSizeMsg{Width: 30, Height: 10})
	await("small terminal", func(f tuiSnapshot) bool { return strings.Contains(f.content, "Enlarge the terminal") })
	program.Send(tea.WindowSizeMsg{Width: 120, Height: 40})
	await("resize recovery", func(f tuiSnapshot) bool { return strings.Contains(f.content, "Details") })
	send("\x1b")
	await("close details", func(f tuiSnapshot) bool { return f.popup == popupNone })
	send("r")
	await("run again", func(f tuiSnapshot) bool { return f.seq == 2 && f.phase == goclient.PhaseMeasuring && !f.finished })
	send("\x1b")
	await("stop prompt", func(f tuiSnapshot) bool { return strings.Contains(f.content, "Stop the test?") })
	send("\x1b")
	await("stopped", func(f tuiSnapshot) bool { return f.finished && f.outcome == goclient.OutcomeStopped })
	send("q")
	select {
	case end := <-done:
		if end.err != nil {
			t.Fatal(end.err)
		}
		final := end.model.(observedTUI).model
		if final.last != goclient.OutcomeStopped || !strings.Contains(ansi.Strip(final.finalReport()), "Stopped") {
			t.Fatal("stopped run lost its report")
		}
	case <-ctx.Done():
		t.Fatal(ctx.Err())
	}
	t.Logf("terminal output: %d bytes", output.n.Load())
}
