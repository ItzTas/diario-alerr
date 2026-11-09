package health

import (
	"context"
)

const staleAfterIntervals = 3

type Service struct {
	repo Repository
	opts Options
}

func NewService(repo Repository, opts Options) *Service {
	return &Service{repo: repo, opts: opts}
}

func (s *Service) Healthy(ctx context.Context) (bool, error) {
	last, err := s.repo.LastSuccess(ctx)
	if err != nil {
		return false, err
	}
	reference := s.opts.Started
	if last != nil {
		reference = *last
	}
	return s.opts.Now().Sub(reference) < staleAfterIntervals*s.opts.Interval, nil
}
