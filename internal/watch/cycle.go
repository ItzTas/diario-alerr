package watch

import (
	"context"
	"time"
)

type Cycle struct {
	service  *Service
	interval time.Duration
}

func NewCycle(service *Service, interval time.Duration) *Cycle {
	return &Cycle{service: service, interval: interval}
}

func (c *Cycle) Run(ctx context.Context) error {
	for {
		c.service.RunCycle(ctx)
		select {
		case <-ctx.Done():
			return nil
		case <-time.After(c.interval):
		}
	}
}
