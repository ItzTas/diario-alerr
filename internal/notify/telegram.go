package notify

import (
	"context"
	"fmt"
	"html"
	"net/http"

	"github.com/ItzTas/diario-alerr/internal/core"
)

type Telegram struct {
	Client *http.Client
	APIURL string
	Token  string
	ChatID string
}

func (t Telegram) Send(ctx context.Context, a core.Alert) error {
	text := fmt.Sprintf("🚨 <b>%s</b>\n\n<pre>%s</pre>\n<a href=\"%s\">Open the PDF at the page</a>",
		html.EscapeString(a.Title), html.EscapeString(a.Body), html.EscapeString(a.Link))
	payload := map[string]any{
		"chat_id":              t.ChatID,
		"text":                 text,
		"parse_mode":           "HTML",
		"link_preview_options": map[string]bool{"is_disabled": true},
	}
	return core.PostJSON(ctx, t.Client, t.APIURL+"/bot"+t.Token+"/sendMessage", payload, nil)
}
