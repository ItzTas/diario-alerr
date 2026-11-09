package notify

import (
	"context"
	"net/http"

	"github.com/ItzTas/diario-alerr/internal/core"
)

type Sender func(ctx context.Context, a core.Alert) error

func Channels(cfg core.Config, client *http.Client) map[string]Sender {
	telegram := Telegram{Client: client, APIURL: cfg.TelegramAPIURL, Token: cfg.TelegramToken, ChatID: cfg.TelegramChatID}
	ntfy := Ntfy{Client: client, URL: cfg.NtfyURL, Topic: cfg.NtfyTopic, Token: cfg.NtfyToken}
	all := map[string]struct {
		enabled bool
		send    Sender
	}{
		"telegram": {cfg.TelegramToken != "" && cfg.TelegramChatID != "", telegram.Send},
		"ntfy":     {cfg.NtfyURL != "" && cfg.NtfyTopic != "", ntfy.Send},
	}
	enabled := map[string]Sender{}
	for name, channel := range all {
		if channel.enabled {
			enabled[name] = channel.send
		}
	}
	return enabled
}
