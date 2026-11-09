package health

import "time"

type Options struct {
	Started  time.Time
	Interval time.Duration
	Now      func() time.Time
}
