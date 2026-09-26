package goclient

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

func TestNativeCatalogSelectionAndReconciliation(t *testing.T) {
	t.Parallel()
	a, b := coordinatedFixture(t, "a"), coordinatedFixture(t, "b")
	cfg := fixtureConfig(a)
	cfg.Stages = StageSet{Download: true}
	prepared := prepareFixtureRun(t, cfg, a, b)
	if !slices.Equal(prepared.SelectedIDs(), []string{"self", "b"}) {
		t.Fatal(prepared.SelectedIDs())
	}
	cfg.ServerIDs = []string{"b"}
	selected, err := prepareRun(t.Context(), cfg, nil, nil)
	if err != nil || !slices.Equal(selected.SelectedIDs(), []string{"b"}) {
		t.Fatalf("deselect self: %v %+v", err, selected)
	}
	if selected.Servers[0].Server.Name != "b" || selected.Catalog.Servers[1].Name != "b" {
		t.Fatal("discovery metadata did not replace the catalogue fallback")
	}
	if !selected.FreshFor(cfg) {
		t.Fatal("fresh selection rejected")
	}
	moved := *selected
	moved.Servers = []PreparedServer{{Server: wire.ServerEntry{ID: "b", URL: "https://previous.example"},
		Connection: &PreparedConnection{}}}
	if _, err = prepareRun(t.Context(), cfg, &moved, nil); err == nil ||
		!strings.Contains(err.Error(), "changed origin") {
		t.Fatalf("replaced identity accepted: %v", err)
	}
	cfg.ServerIDs = []string{"removed"}
	if _, err = prepareRun(t.Context(), cfg, nil, nil); err == nil {
		t.Fatal("removed server accepted")
	}
	cfg.ServerIDs = []string{"self", "b"}
	cfg.ThroughputProtocol = "http3"
	unsupported, err := prepareRun(t.Context(), cfg, nil, nil)
	if err == nil || unsupported.Ready() {
		t.Fatal("forced protocol silently downgraded")
	}
	cfg.ThroughputProtocol = "auto"
	cfg.ThroughputTarget = b.server.URL
	if _, err = prepareRun(t.Context(), cfg, nil, nil); err == nil {
		t.Fatal("multi-server origin override accepted")
	}
}

func TestNativeDiscoveryCancellationAndRedirect(t *testing.T) {
	t.Parallel()
	entered, left := make(chan struct{}), make(chan struct{})
	remote := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		close(entered)
		<-r.Context().Done()
		close(left)
	}))
	defer remote.Close()
	a := coordinatedFixture(t, "a")
	a.catalog = wire.ServerCatalog{
		DefaultSelection: []string{"slow"},
		Servers: []wire.ServerEntry{
			{ID: "self", URL: ".", Name: "A"},
			{ID: "slow", URL: remote.URL, Name: "Slow"},
		},
	}
	cfg := fixtureConfig(a)
	cfg.Stages = StageSet{Download: true}
	ctx, cancel := context.WithCancel(t.Context())
	done := make(chan error, 1)
	go func() { _, err := prepareRun(ctx, cfg, nil, nil); done <- err }()
	<-entered
	cancel()
	select {
	case err := <-done:
		if !errors.Is(err, context.Canceled) {
			t.Fatal(err)
		}
	case <-time.After(time.Second):
		t.Fatal("preparation retained cancelled discovery")
	}
	select {
	case <-left:
	case <-time.After(time.Second):
		t.Fatal("remote request survived cancellation")
	}
	redirect := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		http.Redirect(w, r, a.server.URL+"/servers", http.StatusFound)
	}))
	defer redirect.Close()
	cfg.BaseURL = redirect.URL
	reads := a.catalogReads.Load()
	if _, err := prepareRun(t.Context(), cfg, nil, nil); err == nil || a.catalogReads.Load() != reads {
		t.Fatalf("catalogue redirect reached another operator: %v", err)
	}
}

// Each selected server is prepared with its own grant, never the catalogue's or a peer's.
func TestPreparedServersCarryOnlyTheirOwnGrant(t *testing.T) {
	t.Parallel()
	a := coordinatedFixture(t, "a")
	grants := map[string]string{"https://127.0.0.1:1": "grant-b", "https://127.0.0.1:2": "grant-c"}
	a.catalog = wire.ServerCatalog{
		DefaultSelection: []string{"self", "b", "c"},
		Servers: []wire.ServerEntry{
			{ID: "self", URL: ".", Name: "A"},
			{ID: "b", URL: "https://127.0.0.1:1", Name: "B"},
			{ID: "c", URL: "https://127.0.0.1:2", Name: "C"},
		},
	}
	cfg := fixtureConfig(a)
	cfg.Stages = StageSet{Download: true}
	prepared, _ := prepareRun(t.Context(), cfg, nil, grants)
	if prepared == nil || len(prepared.Servers) != 3 {
		t.Fatalf("prepared %+v, want all three selected servers", prepared)
	}
	for _, server := range prepared.Servers {
		if got := server.credential.token; got != grants[server.Server.URL] {
			t.Errorf("%s prepared with grant %q, want %q", server.Server.ID, got, grants[server.Server.URL])
		}
	}
}

func TestNativeFourParticipantsAndCancellation(t *testing.T) {
	t.Parallel()
	fixtures := []*serverFixture{}
	for i := range 4 {
		fixtures = append(fixtures, coordinatedFixture(t, fmt.Sprint(i)))
	}
	catalog := wire.ServerCatalog{}
	for i, f := range fixtures {
		id := fmt.Sprint(i)
		if i == 0 {
			id = "self"
		}
		catalog.DefaultSelection = append(catalog.DefaultSelection, id)
		catalog.Servers = append(catalog.Servers, wire.ServerEntry{ID: id, URL: f.server.URL, Name: id})
	}
	fixtures[0].catalog = catalog
	cfg := fixtureConfig(fixtures[0])
	cfg.Stages = StageSet{Bidirectional: true}
	cfg.BidirectionalDuration = 5 * time.Second
	prepared, err := prepareRun(t.Context(), cfg, nil, nil)
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	var details *RunDetails
	doneCount := 0
	err = runSelected(ctx, cfg, prepared, func(e Event) {
		if e.Kind == EventStage && e.Phase == PhaseMeasuring {
			time.AfterFunc(time.Second, cancel)
		}
		if e.Servers != nil {
			details = e.Servers
		}
		if e.Kind == EventDone {
			doneCount++
		}
	})
	if !errors.Is(err, context.Canceled) ||
		doneCount != 1 ||
		details == nil ||
		len(details.Servers) != 4 ||
		details.Outcome != OutcomeStopped {
		t.Fatalf("cancelled result: %v %+v done=%d", err, details, doneCount)
	}
	for _, f := range fixtures {
		released := make(chan struct{})
		go func() {
			f.handlers.Wait()
			f.conns.Wait()
			close(released)
		}()
		select {
		case <-released:
		case <-time.After(10 * time.Second):
			t.Fatal("a participant request or connection survived cancellation")
		}
	}
}

func TestNativeLaterCheckpointFailureKeepsSurvivor(t *testing.T) {
	t.Parallel()
	a, b := coordinatedFixture(t, "a"), coordinatedFixture(t, "b")
	cfg := fixtureConfig(a)
	cfg.Stages = StageSet{Latency: true, Upload: true}
	cfg.LatencyDuration = 150 * time.Millisecond
	prepared := prepareFixtureRun(t, cfg, a, b)
	var details *RunDetails
	var upload Result
	err := runSelected(t.Context(), cfg, prepared, func(e Event) {
		if e.Kind == EventStage && e.Stage == "latency" && e.Phase == PhaseFinished {
			a.checkpointFailed.Store(true)
		}
		if e.Servers != nil {
			details = e.Servers
		}
		if e.Kind == EventResult && e.ServerID == "" && e.Direction == Up {
			upload = *e.Result
		}
	})
	if err != nil ||
		upload.Unavailable ||
		upload.MeanBps <= 0 ||
		details == nil ||
		!slices.Equal(details.Participants, []string{"b"}) ||
		len(details.Failures) != 1 {
		t.Fatalf("later preparation discarded healthy server: %v %+v %+v", err, upload, details)
	}
}
