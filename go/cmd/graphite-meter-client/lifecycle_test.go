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
	defer m.controller.Close()
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
	defer m.controller.Close()
	m, _ = modelAndCmd(m.reprepare(nil))
	command := pollAuthorization(m.preparation, m.prepareSeq, pending)
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
			defer m.controller.Close()
			m, _ = modelAndCmd(m.reprepare(nil))
			preparation := m.preparation
			if editing {
				m.edit = beginEdit(editURL, "url", m.cfg.BaseURL)
			}
			_, command := modelAndCmd(m.Update(tea.KeyMsg{Type: tea.KeyCtrlC}))
			if _, ok := command().(tea.QuitMsg); !ok {
				t.Fatal("quit key did not quit")
			}
			if _, err := preparation.Prepare(); !errors.Is(err, context.Canceled) {
				t.Fatal("quit left the queued preparation active")
			}
			if _, err := m.controller.NewPreparation(m.cfg).Prepare(); !errors.Is(err, context.Canceled) {
				t.Fatal("quit allowed new preparation work")
			}
		})
	}
}

func TestApplicationShutdownCancelsWorkOwnedByUpdatedModel(t *testing.T) {
	initial := newModel(goclient.DefaultConfig())
	updated, _ := modelAndCmd(initial.reprepare(nil))
	initial.controller.Close()
	if _, err := updated.preparation.Prepare(); !errors.Is(err, context.Canceled) {
		t.Fatal("program exit did not cancel work created by an updated model")
	}
}

func TestStartingRunCancelsPreparationAndItsQueuedMessages(t *testing.T) {
	cfg := goclient.DefaultConfig()
	cfg.BaseURL = ":invalid"
	m := newModel(cfg)
	defer m.controller.Close()
	m, _ = modelAndCmd(m.reprepare(nil))
	preparation, seq := m.preparation, m.prepareSeq
	m, _ = m.startRun()
	if _, err := preparation.Prepare(); !errors.Is(err, context.Canceled) || m.prepareSeq == seq {
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

// The key tests exercise an actual owned request rather than injecting a cancel callback.
func startPendingRun(t *testing.T) (model, <-chan struct{}) {
	t.Helper()
	entered, left := make(chan struct{}), make(chan struct{})
	srv := httptest.NewServer(http.HandlerFunc(func(_ http.ResponseWriter, r *http.Request) {
		close(entered)
		<-r.Context().Done()
		close(left)
	}))
	t.Cleanup(srv.Close)
	cfg := goclient.DefaultConfig()
	cfg.BaseURL = srv.URL
	m := newModel(cfg)
	t.Cleanup(m.controller.Close)
	m, _ = m.startRun()
	select {
	case <-entered:
	case <-time.After(2 * time.Second):
		t.Fatal("run never reached preparation")
	}
	return m, left
}
