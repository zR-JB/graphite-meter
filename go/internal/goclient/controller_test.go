package goclient

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"sync"
	"testing"
	"time"
)

func TestControllerCloseRejectsQueuedAndConcurrentPreparation(t *testing.T) {
	t.Parallel()
	srv := httptest.NewServer(http.HandlerFunc(func(_ http.ResponseWriter, r *http.Request) {
		_, _ = io.Copy(io.Discard, r.Body)
		<-r.Context().Done()
	}))
	defer srv.Close()
	cfg := DefaultConfig()
	cfg.BaseURL = srv.URL
	for range 20 {
		owner := NewController(t.Context())
		preparation := owner.NewPreparation(cfg)
		pending := &PendingAuthorization{tokenURL: srv.URL, client: srv.Client(), close: func() {}}
		start := make(chan struct{})
		var work sync.WaitGroup
		work.Go(func() { <-start; _, _ = preparation.PrepareRun() })
		work.Go(func() { <-start; _, _ = preparation.PollAuthorization(pending) })
		work.Go(func() { <-start; owner.Close() })
		close(start)
		work.Wait()
		for _, token := range []*Preparation{preparation, owner.NewPreparation(cfg)} {
			if _, err := token.PrepareRun(); !errors.Is(err, context.Canceled) {
				t.Fatalf("closed preparation started work: %v", err)
			}
			if _, err := token.PollAuthorization(pending); !errors.Is(err, context.Canceled) {
				t.Fatalf("closed approval started work: %v", err)
			}
			if _, err := token.BeginAuthorization("", "https://meter.test/login"); !errors.Is(err, context.Canceled) {
				t.Fatalf("closed preparation created an approval: %v", err)
			}
		}
		if _, ok := <-owner.Start(cfg, nil); ok {
			t.Fatal("closed controller started a run")
		}
	}
}

func waitControllerWork(t *testing.T, owner *Controller) {
	t.Helper()
	done := make(chan struct{})
	go func() { owner.work.Wait(); close(done) }()
	select {
	case <-done:
	case <-time.After(2 * time.Second):
		owner.Close()
		t.Fatal("abandoned event delivery retained native work")
	}
}

func TestControllerRunCancellationAndAbandonment(t *testing.T) {
	t.Parallel()
	for _, operation := range []string{"cancel", "replace", "close"} {
		t.Run(operation, func(t *testing.T) {
			t.Parallel()
			srv := newTransferServer(t)
			defer srv.Close()
			cfg := Config{
				BaseURL:         srv.URL,
				Stages:          StageSet{Latency: true},
				LatencyDuration: 10 * time.Second,
				PingInterval:    time.Millisecond,
			}
			owner := NewController(t.Context())
			defer owner.Close()
			events := owner.Start(cfg, nil)
			deadline := time.NewTimer(3 * time.Second)
			defer deadline.Stop()
			ticks := time.Tick(time.Millisecond)
			for len(events) != cap(events) {
				select {
				case <-ticks:
				case <-deadline.C:
					t.Fatal("measurement never filled its bounded event stream")
				}
			}
			switch operation {
			case "cancel":
				owner.CancelRun()
				var done []Event
				for event := range events {
					if event.Kind == EventDone {
						done = append(done, event)
					}
				}
				if len(done) != 1 ||
					!errors.Is(done[0].Err, context.Canceled) ||
					done[0].Outcome() != OutcomeStopped ||
					len(done[0].Servers.Servers) != 1 {
					t.Fatalf("user cancellation lost its terminal outcome: %+v", done)
				}
				results := done[0].Servers.Servers[0].Results
				if len(results) != 1 || results[0].Latency.Count == 0 || !errors.Is(results[0].Err, context.Canceled) {
					t.Fatalf("user cancellation lost final evidence: %+v", results)
				}
			case "replace":
				cfg.BaseURL = ":invalid"
				replacement := owner.Start(cfg, nil)
				var terminal bool
				for event := range replacement {
					terminal = terminal || event.Kind == EventDone
				}
				if !terminal {
					t.Fatal("replacement did not retain its own terminal event")
				}
				// Keep the old queue full: replacement itself must release the old producer.
				waitControllerWork(t, owner)
			case "close":
				closed := make(chan struct{})
				go func() { owner.Close(); close(closed) }()
				select {
				case <-closed:
				case <-time.After(2 * time.Second):
					t.Fatal("close did not abandon its full event stream")
				}
			}
		})
	}
}

func TestRunAbortDrainsResultsAndReplacementUnblocksDelivery(t *testing.T) {
	t.Parallel()
	measurement, abort := context.WithCancel(t.Context())
	abort()
	for _, terminal := range []Event{
		{Kind: EventResult},
		{Kind: EventDone, Servers: &RunDetails{Outcome: OutcomeStopped}},
	} {
		t.Run(fmt.Sprint(terminal.Kind), func(t *testing.T) {
			t.Parallel()
			delivery, abandon := context.WithCancel(t.Context())
			defer abandon()
			// Exercise both ready select arms: cancellation must never compete
			// with a terminal record, even when delivery has spare capacity.
			for range 64 {
				available := make(chan Event, 1)
				sendRunEvent(measurement, delivery, available, terminal)
				if len(available) != 1 {
					t.Fatal("user cancellation discarded an immediately deliverable terminal outcome")
				}
			}
			events := make(chan Event, 1)
			events <- Event{Kind: EventLatency}
			sent := make(chan struct{})
			go func() {
				sendRunEvent(measurement, delivery, events, terminal)
				close(sent)
			}()
			<-events
			select {
			case event := <-events:
				if event.Kind != terminal.Kind || event.Servers != terminal.Servers {
					t.Fatalf("delivered %+v, want %+v", event, terminal)
				}
			case <-time.After(time.Second):
				t.Fatal("user cancellation discarded the queued final outcome")
			}
			<-sent
			events <- Event{}
			abandon()
			// A superseded run can exit even when its consumer has stopped draining the full queue.
			sendRunEvent(measurement, delivery, events, terminal)
		})
	}
}

func TestPreparationReplacementCancelsActiveApprovalRequest(t *testing.T) {
	t.Parallel()
	entered, left := make(chan struct{}), make(chan struct{})
	srv := httptest.NewServer(http.HandlerFunc(func(_ http.ResponseWriter, r *http.Request) {
		_, _ = io.Copy(io.Discard, r.Body)
		close(entered)
		<-r.Context().Done()
		close(left)
	}))
	defer srv.Close()
	owner := NewController(t.Context())
	defer owner.Close()
	preparation := owner.NewPreparation(DefaultConfig())
	pending := &PendingAuthorization{tokenURL: srv.URL, client: srv.Client(), close: func() {}}
	done := make(chan error, 1)
	go func() { _, err := preparation.PollAuthorization(pending); done <- err }()
	select {
	case <-entered:
	case <-time.After(time.Second):
		t.Fatal("approval did not reach the server")
	}
	owner.NewPreparation(DefaultConfig())
	select {
	case err := <-done:
		if !errors.Is(err, context.Canceled) {
			t.Fatalf("superseded approval returned %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("superseded approval kept polling")
	}
	select {
	case <-left:
	case <-time.After(time.Second):
		t.Fatal("approval request survived preparation replacement")
	}
}

func TestLiveSamplesNeverBlockOnAFullView(t *testing.T) {
	t.Parallel()
	events := make(chan Event, 1)
	events <- Event{}
	delivery, abandon := context.WithCancel(t.Context())
	sendRunEvent(t.Context(), delivery, events, Event{Kind: EventLatency})
	sendRunEvent(t.Context(), delivery, events, Event{Kind: EventThroughput})
	delivered := make(chan struct{})
	go func() {
		sendRunEvent(t.Context(), delivery, events, Event{Kind: EventResult})
		close(delivered)
	}()
	select {
	case <-delivered:
		t.Fatal("a result was dropped by a full view")
	case <-time.After(20 * time.Millisecond):
	}
	abandon()
	<-delivered
}
