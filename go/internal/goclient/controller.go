package goclient

import (
	"context"
	"errors"
	"maps"
	"slices"
	"sync"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/wire"
)

const (
	preparationTimeout = 12 * time.Second
	runEventCapacity   = 256
)

// AuthorizationTimeout bounds approval polling and its displayed countdown.
const AuthorizationTimeout = 2 * time.Minute

// Controller owns preparation, approval polling, grants, and measurement lifetimes for one client.
// UI sequence guards still decide whether an already queued reply belongs to the current view.
type Controller struct {
	mu          sync.Mutex
	catalog     *wire.ServerCatalog
	selection   []wire.ServerEntry
	grants      map[string]string // Bearer grants by their issuer's canonical origin; the only copy.
	ctx         context.Context
	cancel      context.CancelFunc
	preparation context.CancelFunc
	run         *activeRun
	work        sync.WaitGroup
}

type activeRun struct {
	cancel  context.CancelFunc
	abandon context.CancelFunc
}

func NewController(parent context.Context) *Controller {
	ctx, cancel := context.WithCancel(parent)
	return &Controller{ctx: ctx, cancel: cancel, grants: map[string]string{}}
}

// Preparation captures the configuration and cancellation scope of delayed UI commands.
type Preparation struct {
	owner *Controller
	ctx   context.Context
	cfg   Config
}

// NewPreparation cancels the previous preparation, including its approval polling.
func (c *Controller) NewPreparation(cfg Config) *Preparation {
	cfg.ServerIDs = slices.Clone(cfg.ServerIDs)
	c.mu.Lock()
	defer c.mu.Unlock()
	c.cancelPreparation()
	if c.ctx.Err() != nil {
		return &Preparation{owner: c, ctx: c.ctx, cfg: cfg}
	}
	ctx, cancel := context.WithCancel(c.ctx)
	c.preparation = cancel
	return &Preparation{owner: c, ctx: ctx, cfg: cfg}
}

func (c *Controller) cancelPreparation() {
	if c.preparation != nil {
		c.preparation()
		c.preparation = nil
	}
}

func (p *Preparation) begin(timeout time.Duration) (context.Context, func(), error) {
	p.owner.mu.Lock()
	defer p.owner.mu.Unlock()
	if err := p.ctx.Err(); err != nil {
		return nil, nil, err
	}
	p.owner.work.Add(1)
	ctx, cancel := context.WithTimeout(p.ctx, timeout)
	return ctx, func() {
		cancel()
		p.owner.work.Done()
	}, nil
}

// PrepareRun loads the catalogue and checks every selected server's paths with the controller's grants.
func (p *Preparation) PrepareRun() (*PreparedRun, error) {
	ctx, done, err := p.begin(preparationTimeout)
	if err != nil {
		return nil, err
	}
	defer done()
	previous, grants := p.owner.snapshot()
	prepared, err := prepareRun(ctx, p.cfg, previous, grants)
	p.owner.mu.Lock()
	defer p.owner.mu.Unlock()
	if prepared != nil && p.ctx.Err() == nil {
		p.owner.catalog = new(prepared.Catalog)
		if prepared.Ready() {
			p.owner.selection = nil
			for _, server := range prepared.Servers {
				p.owner.selection = append(p.owner.selection, server.Server)
			}
		}
	}
	return prepared, err
}

func (c *Controller) snapshot() ([]wire.ServerEntry, map[string]string) {
	c.mu.Lock()
	defer c.mu.Unlock()
	return slices.Clone(c.selection), maps.Clone(c.grants)
}

// BeginAuthorization starts approval with the server that issued the challenge: the catalogue origin
// when serverID is empty, otherwise that catalogue entry.
func (p *Preparation) BeginAuthorization(serverID, authURL string) (*PendingAuthorization, error) {
	if err := p.ctx.Err(); err != nil {
		return nil, err
	}
	cfg := p.cfg
	p.owner.mu.Lock()
	found := serverID == ""
	if p.owner.catalog != nil && !found {
		for _, server := range p.owner.catalog.Servers {
			if server.ID == serverID {
				cfg.BaseURL, found = server.URL, true
			}
		}
	}
	p.owner.mu.Unlock()
	if !found {
		return nil, errors.New("server is no longer in the catalogue")
	}
	return beginAuthorization(cfg, authURL)
}

func (p *Preparation) PollAuthorization(pending *PendingAuthorization) (string, error) {
	ctx, done, err := p.begin(AuthorizationTimeout)
	if err != nil {
		return "", err
	}
	defer done()
	return pending.Poll(ctx)
}

// AcceptAuthorization keeps a native grant in memory, indexed by its exact issuer origin.
func (c *Controller) AcceptAuthorization(origin, token string) error {
	canonical, err := wire.CanonicalOrigin(origin)
	if err != nil || canonical != origin {
		return errors.New("invalid authorization origin")
	}
	if token == "" || len(token) > 8192 {
		return errors.New("invalid authorization grant")
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	if _, exists := c.grants[origin]; !exists && len(c.grants) >= wire.MaxCatalogServers {
		return errors.New("too many authorized servers; restart the client to clear unused grants")
	}
	c.grants[origin] = token
	return nil
}

// SelectServers acknowledges the identities displayed by the most recently loaded catalogue.
func (c *Controller) SelectServers(ids []string) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.catalog == nil {
		return errors.New("server catalogue is unavailable")
	}
	if err := c.catalog.ValidateSelection(ids); err != nil {
		return err
	}
	c.selection = nil
	for _, server := range c.catalog.Servers {
		if slices.Contains(ids, server.ID) {
			c.selection = append(c.selection, server)
		}
	}
	return nil
}

// Start abandons a replaced run's delivery, then runs the selection with one bounded event stream.
// A stale preparation is repeated first; the stream always ends with one EventDone.
func (c *Controller) Start(cfg Config, prepared *PreparedRun) <-chan Event {
	previous, grants := c.snapshot()
	c.mu.Lock()
	defer c.mu.Unlock()
	c.cancelPreparation()
	if c.run != nil {
		c.run.abandon()
	}
	events := make(chan Event, runEventCapacity)
	if c.ctx.Err() != nil {
		close(events)
		return events
	}
	// Delivery outlives measurement, so a cancelled run still reports its results and teardown still ends on Close.
	delivery, abandon := context.WithCancel(c.ctx)
	measurement, cancel := context.WithCancel(delivery)
	c.run = &activeRun{cancel: cancel, abandon: abandon}
	c.work.Go(func() {
		defer cancel()
		defer abandon()
		defer close(events)
		emit := func(event Event) { sendRunEvent(measurement, delivery, events, event) }
		if !prepared.FreshFor(cfg) {
			ctx, cancelPreparation := context.WithTimeout(measurement, preparationTimeout)
			var err error
			prepared, err = prepareRun(ctx, cfg, previous, grants)
			cancelPreparation()
			if err != nil {
				emit(Event{Kind: EventDone, At: time.Now(), Err: err})
				return
			}
		}
		_ = runSelection(measurement, delivery, cfg, prepared, emit)
	})
	return events
}

// Run is Start for callers without a view: it prepares the selection, measures it to completion, and
// delivers every event to emit on the calling goroutine. It returns the terminal event's error.
func Run(ctx context.Context, cfg Config, emit func(Event)) error {
	controller := NewController(ctx)
	defer controller.Close()
	var err error
	for event := range controller.Start(cfg, nil) {
		if event.Kind == EventDone {
			err = event.Err
		}
		emit(event)
	}
	return err
}

// CancelRun stops measurement while retaining its final results and terminal event.
func (c *Controller) CancelRun() {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.run != nil {
		c.run.cancel()
	}
}

// Close abandons queued delivery and joins started work, including work from replaced scopes.
func (c *Controller) Close() {
	c.mu.Lock()
	c.cancel()
	c.mu.Unlock()
	c.work.Wait()
}

func sendRunEvent(measurement, delivery context.Context, events chan<- Event, event Event) {
	switch event.Kind {
	case EventThroughput, EventLatency:
		// A live sample never delays the reader or timer that produced it; a slow view drops it.
		select {
		case events <- event:
		default:
		}
		return
	case EventResult, EventDone:
		// Outcomes survive cancelled measurement and wait until delivery is abandoned.
		measurement = delivery
	}
	select {
	case events <- event:
	case <-measurement.Done():
	}
}
