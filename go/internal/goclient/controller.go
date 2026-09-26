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

const AuthorizationTimeout = 2 * time.Minute

type Controller struct {
	mu          sync.Mutex
	grants      map[string]string
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

type Preparation struct {
	owner    *Controller
	ctx      context.Context
	cfg      Config
	previous *PreparedRun
}

func (c *Controller) NewPreparation(cfg Config, previous *PreparedRun) *Preparation {
	cfg.ServerIDs = slices.Clone(cfg.ServerIDs)
	c.mu.Lock()
	defer c.mu.Unlock()
	c.cancelPreparation()
	p := &Preparation{owner: c, ctx: c.ctx, cfg: cfg, previous: previous}
	if c.ctx.Err() == nil {
		p.ctx, c.preparation = context.WithCancel(c.ctx)
	}
	return p
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

func (p *Preparation) PrepareRun() (*PreparedRun, error) {
	ctx, done, err := p.begin(preparationTimeout)
	if err != nil {
		return nil, err
	}
	defer done()
	return prepareRun(ctx, p.cfg, p.previous, p.owner.snapshot())
}

func (c *Controller) snapshot() map[string]string {
	c.mu.Lock()
	defer c.mu.Unlock()
	return maps.Clone(c.grants)
}

func (p *Preparation) BeginAuthorization(origin, authURL string) (*PendingAuthorization, error) {
	if err := p.ctx.Err(); err != nil {
		return nil, err
	}
	cfg := p.cfg
	cfg.BaseURL = origin
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

func (c *Controller) Start(cfg Config, prepared *PreparedRun) <-chan Event {
	grants := c.snapshot()
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
			prepared, err = prepareRun(ctx, cfg, prepared, grants)
			cancelPreparation()
			if err != nil && !prepared.runnable() {
				emit(Event{Kind: EventDone, At: time.Now(), Err: err})
				return
			}
		}
		runSelection(measurement, delivery, cfg, prepared, emit)
	})
	return events
}

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

func (c *Controller) CancelRun() {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.run != nil {
		c.run.cancel()
	}
}

func (c *Controller) Close() {
	c.mu.Lock()
	c.cancel()
	c.mu.Unlock()
	c.work.Wait()
}

func sendRunEvent(measurement, delivery context.Context, events chan<- Event, event Event) {
	switch event.Kind {
	case EventThroughput, EventLatency:
		select {
		case events <- event:
		default:
		}
		return
	case EventResult, EventDone:
		measurement = delivery
	}
	select {
	case events <- event:
	case <-measurement.Done():
	}
}
