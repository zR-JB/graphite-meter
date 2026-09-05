package main

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	tea "github.com/charmbracelet/bubbletea"
	"github.com/zR-JB/graphite-meter/go/internal/goclient"
)

func TestReprepareCancelsActiveRequestAndDiscardsItsReply(t *testing.T) {
	entered := make(chan struct{})
	left := make(chan struct{})
	srv := httptest.NewServer(http.HandlerFunc(func(_ http.ResponseWriter, r *http.Request) {
		close(entered)
		<-r.Context().Done()
		close(left)
	}))
	defer srv.Close()
	cfg := goclient.DefaultConfig()
	cfg.BaseURL = srv.URL
	m := newModel(cfg)
	defer m.shutdown()
	m, _ = modelAndCmd(m.reprepare(nil))
	m, command := modelAndCmd(m.handlePrepareDue(prepareDueMsg{seq: m.prepareSeq}))
	replied := make(chan tea.Msg, 1)
	go func() { replied <- command() }()
	select {
	case <-entered:
	case <-time.After(2 * time.Second):
		t.Fatal("preparation never reached the server")
	}
	m.cfg.BaseURL = "http://new-server.test"
	m, _ = modelAndCmd(m.reprepare(nil))
	select {
	case <-left:
	case <-time.After(2 * time.Second):
		t.Fatal("superseded preparation kept its HTTP request alive")
	}
	select {
	case reply := <-replied:
		if !errors.Is(reply.(preparationMsg).err, context.Canceled) {
			t.Fatalf("preparation error = %v, want cancellation", reply)
		}
		m, next := modelAndCmd(m.Update(reply))
		if next != nil || m.prepareStatus != "checking" || m.prepareError != "" {
			t.Fatalf("stale reply changed the new preparation: %+v", m)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("superseded command did not finish")
	}
}

func TestPreparationCancellationReachesQueuedAuthorizationPoll(t *testing.T) {
	cfg := goclient.DefaultConfig()
	cfg.BaseURL = "https://meter.test"
	pending, err := goclient.BeginAuthorization(cfg, cfg.BaseURL+"/login")
	if err != nil {
		t.Fatal(err)
	}
	m := newModel(cfg)
	defer m.shutdown()
	m, _ = modelAndCmd(m.reprepare(nil))
	command := pollAuthorization(m.prepareCtx, m.prepareSeq, pending)
	m, _ = modelAndCmd(m.reprepare(nil))
	reply := command().(authTokenMsg)
	if !errors.Is(reply.err, context.Canceled) || reply.token != "" {
		t.Fatalf("canceled authorization = %+v", reply)
	}
	_, next := modelAndCmd(m.Update(reply))
	if next != nil {
		t.Fatal("stale authorization launched more work")
	}
}

func TestQuitCancelsPreparationInConfigurationAndEditor(t *testing.T) {
	for _, editing := range []bool{false, true} {
		t.Run(fmt.Sprint(editing), func(t *testing.T) {
			m := newModel(goclient.DefaultConfig())
			defer m.shutdown()
			m, _ = modelAndCmd(m.reprepare(nil))
			ctx := m.prepareCtx
			if editing {
				m.edit = beginEdit(editURL, "url", m.cfg.BaseURL)
			}
			_, command := modelAndCmd(m.Update(tea.KeyMsg{Type: tea.KeyCtrlC}))
			if _, ok := command().(tea.QuitMsg); !ok {
				t.Fatal("quit key did not quit")
			}
			if ctx.Err() != context.Canceled || m.lifetime.Err() != context.Canceled {
				t.Fatal("quit left preparation or application lifetime active")
			}
		})
	}
}

func TestApplicationShutdownCancelsWorkOwnedByUpdatedModel(t *testing.T) {
	initial := newModel(goclient.DefaultConfig())
	updated, _ := modelAndCmd(initial.reprepare(nil))
	initial.shutdown()
	if updated.prepareCtx.Err() != context.Canceled {
		t.Fatal("program exit did not cancel work created by an updated model")
	}
}

func TestStartingRunCancelsPreparationAndItsQueuedMessages(t *testing.T) {
	cfg := goclient.DefaultConfig()
	cfg.BaseURL = ":invalid"
	m := newModel(cfg)
	defer m.shutdown()
	m, _ = modelAndCmd(m.reprepare(nil))
	ctx, seq := m.prepareCtx, m.prepareSeq
	m, _ = m.startRun()
	if ctx.Err() != context.Canceled || m.prepareSeq == seq {
		t.Fatal("starting a run left its preparation active")
	}
	if _, command := modelAndCmd(m.handlePrepareDue(prepareDueMsg{seq: seq})); command != nil {
		t.Fatal("a queued preparation started during the run")
	}
	drainRun(t, m.events)
}

func drainRun(t *testing.T, events <-chan goclient.Event) {
	t.Helper()
	deadline := time.After(5 * time.Second)
	var last goclient.Event
	terminals := 0
	for {
		select {
		case event, ok := <-events:
			if !ok {
				if terminals != 1 || last.Kind != goclient.EventDone {
					t.Fatalf("stream ended without one final outcome: terminals=%d last=%+v", terminals, last)
				}
				return
			}
			last = event
			if event.Kind == goclient.EventDone {
				terminals++
			}
		case <-deadline:
			t.Fatal("run did not close its event stream")
		}
	}
}

func TestDoneDistinguishesWrappedCancellationFromErrorText(t *testing.T) {
	for _, err := range []error{fmt.Errorf("transfer: %w", context.Canceled), errors.New("server said context canceled")} {
		m := newModel(goclient.DefaultConfig())
		defer m.shutdown()
		m, _ = modelAndCmd(m.handleEvents(eventsMsg{events: []goclient.Event{{Kind: goclient.EventDone, Err: err}}}))
		if errors.Is(err, context.Canceled) {
			if m.err != nil || m.status != "canceled" {
				t.Fatalf("wrapped cancellation became a failure: %v", m.err)
			}
		} else if m.err != err || m.status != "error" {
			t.Fatal("an error mentioning cancellation was mistaken for a user cancellation")
		}
	}
}

func TestRunAbortDrainsResultsAndReplacementUnblocksDelivery(t *testing.T) {
	measurement, abort := context.WithCancel(t.Context())
	abort()
	for _, kind := range []goclient.EventKind{goclient.EventResult, goclient.EventDone} {
		t.Run(fmt.Sprint(kind), func(t *testing.T) {
			delivery, abandon := context.WithCancel(t.Context())
			defer abandon()
			events := make(chan goclient.Event, 1)
			events <- goclient.Event{Kind: goclient.EventLatency}
			sent := make(chan struct{})
			go func() {
				sendRunEvent(measurement, delivery, events, goclient.Event{Kind: kind})
				close(sent)
			}()
			<-events
			select {
			case event := <-events:
				if event.Kind != kind {
					t.Fatalf("delivered %v, want %v", event.Kind, kind)
				}
			case <-time.After(time.Second):
				t.Fatal("user cancellation discarded the queued final outcome")
			}
			<-sent
			events <- goclient.Event{}
			abandon()
			// A superseded run can exit even when its consumer has stopped draining the full queue.
			sendRunEvent(measurement, delivery, events, goclient.Event{Kind: kind})
		})
	}
}
