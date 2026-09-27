package goclient

import (
	"context"
	"errors"
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
		preparation := owner.NewPreparation(cfg, nil)
		pending := &PendingAuthorization{tokenURL: srv.URL, client: srv.Client(), close: func() {}}
		start := make(chan struct{})
		var work sync.WaitGroup
		work.Go(func() { <-start; _, _ = preparation.PrepareRun() })
		work.Go(func() { <-start; _, _ = preparation.PollAuthorization(pending) })
		work.Go(func() { <-start; owner.Close() })
		close(start)
		work.Wait()
		for _, token := range []*Preparation{preparation, owner.NewPreparation(cfg, nil)} {
			if _, err := token.PrepareRun(); !errors.Is(err, context.Canceled) {
				t.Fatalf("closed preparation started work: %v", err)
			}
			if _, err := token.PollAuthorization(pending); !errors.Is(err, context.Canceled) {
				t.Fatalf("closed approval started work: %v", err)
			}
			_, err := token.BeginAuthorization("https://meter.test", "https://meter.test/login")
			if !errors.Is(err, context.Canceled) {
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
				BaseURL:            srv.URL,
				Stages:             StageSet{Latency: true},
				LatencyDuration:    10 * time.Second,
				PingInterval:       time.Millisecond,
				LoadedPingInterval: time.Millisecond,
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

func TestRunEventsKeepFinalRecordsAndDropLiveSamples(t *testing.T) {
	t.Parallel()
	measurement, abort := context.WithCancel(t.Context())
	abort()
	for _, terminal := range []Event{
		{Kind: EventResult},
		{Kind: EventDone, Servers: &RunDetails{Outcome: OutcomeStopped}},
	} {
		// Both select arms are ready: cancellation must never compete with a deliverable terminal record.
		for range 64 {
			available := make(chan Event, 1)
			sendRunEvent(measurement, t.Context(), available, terminal)
			if len(available) != 1 {
				t.Fatalf("user cancellation discarded an immediately deliverable %v", terminal.Kind)
			}
		}
	}
	full := make(chan Event, 1)
	full <- Event{}
	sendRunEvent(t.Context(), t.Context(), full, Event{Kind: EventLatency})
	sendRunEvent(t.Context(), t.Context(), full, Event{Kind: EventThroughput})
}
