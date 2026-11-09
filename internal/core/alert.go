package core

import "time"

const (
	WelcomeAlertID = "welcome"
	SiteURL        = "https://diarios.al.rr.leg.br"
	GazetteName    = "ALE/RR Official Gazette"
)

type Alert struct {
	ID        string
	Title     string
	Link      string
	Body      string
	Published time.Time
}
