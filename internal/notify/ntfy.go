package notify

import (
	"context"
	"net/http"

	"github.com/ItzTas/diario-alerr/internal/core"
)

type Ntfy struct {
	Client *http.Client
	URL    string
	Topic  string
	Token  string
}

func (n Ntfy) Send(ctx context.Context, a core.Alert) error {
	priority := 5
	if a.ID == core.WelcomeAlertID {
		priority = 3
	}
	payload := map[string]any{
		"topic":    n.Topic,
		"title":    a.Title,
		"message":  a.Body,
		"click":    a.Link,
		"tags":     []string{"rotating_light"},
		"priority": priority,
	}
	headers := map[string]string{}
	if n.Token != "" {
		headers["Authorization"] = "Bearer " + n.Token
	}
	return core.PostJSON(ctx, n.Client, n.URL, payload, headers)
}
