package goclient

import (
	"context"
	"sync"
	"time"
)

const (
	preparationTimeout = 12 * time.Second
	runEventCapacity   = 256
)

// AuthorizationTimeout bounds approval polling and its displayed countdown.
const AuthorizationTimeout = 2 * time.Minute

// Controller owns preparation, approval polling, and measurement lifetimes for one client.
// UI sequence guards still decide whether an already queued reply belongs to the current view.
type Controller struct {
	mu          sync.Mutex
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
	return &Controller{ctx: ctx, cancel: cancel}
}

// Preparation captures the configuration and cancellation scope of delayed UI commands.
type Preparation struct {
	owner *Controller
	ctx   context.Context
	cfg   Config
}

func (c *Controller) NewPreparation(cfg Config) *Preparation {
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

func (p *Preparation) Prepare() (*PreparedConnection, error) {
	ctx, done, err := p.begin(preparationTimeout)
	if err != nil {
		return nil, err
	}
	defer done()
	return Prepare(ctx, p.cfg)
}

func (p *Preparation) BeginAuthorization(authURL string) (*PendingAuthorization, error) {
	if err := p.ctx.Err(); err != nil {
		return nil, err
	}
	return BeginAuthorization(p.cfg, authURL)
}

func (p *Preparation) PollAuthorization(pending *PendingAuthorization) (string, error) {
	ctx, done, err := p.begin(AuthorizationTimeout)
	if err != nil {
		return "", err
	}
	defer done()
	return pending.Poll(ctx)
}

// Start abandons a replaced run's delivery, then starts one bounded event stream.
func (c *Controller) Start(cfg Config, prepared *PreparedConnection) <-chan Event {
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
		_ = RunPrepared(measurement, cfg, prepared, func(event Event) {
			if event.Kind == EventDone {
				event.Err = ClassifyAuthFailure(measurement, cfg, event.Err)
			}
			sendRunEvent(measurement, delivery, events, event)
		})
	})
	return events
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
	if event.Kind == EventResult || event.Kind == EventDone {
		measurement = delivery
	}
	select {
	case events <- event:
	case <-measurement.Done():
	}
}
