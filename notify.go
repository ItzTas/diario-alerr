package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"html"
	"io"
	"log/slog"
	"net/http"
	"net/url"
	"slices"
)

type sender func(ctx context.Context, a *alert) error

func (m *monitor) channels() map[string]sender {
	all := map[string]struct {
		enabled bool
		send    sender
	}{
		"telegram": {m.cfg.telegramToken != "" && m.cfg.telegramChatID != "", m.sendTelegram},
		"ntfy":     {m.cfg.ntfyURL != "" && m.cfg.ntfyTopic != "", m.sendNtfy},
	}
	enabled := map[string]sender{}
	for name, channel := range all {
		if channel.enabled {
			enabled[name] = channel.send
		}
	}
	return enabled
}

func (m *monitor) sendTelegram(ctx context.Context, a *alert) error {
	text := fmt.Sprintf("🚨 <b>%s</b>\n\n<pre>%s</pre>\n<a href=\"%s\">Abrir o PDF na página</a>",
		html.EscapeString(a.Title), html.EscapeString(a.Body), html.EscapeString(a.Link))
	payload := map[string]any{
		"chat_id":              m.cfg.telegramChatID,
		"text":                 text,
		"parse_mode":           "HTML",
		"link_preview_options": map[string]bool{"is_disabled": true},
	}
	return postJSON(ctx, m.client, m.telegramAPI+"/bot"+m.cfg.telegramToken+"/sendMessage", payload, nil)
}

func (m *monitor) sendNtfy(ctx context.Context, a *alert) error {
	priority := 5
	if a.ID == welcomeID {
		priority = 3
	}
	payload := map[string]any{
		"topic":    m.cfg.ntfyTopic,
		"title":    a.Title,
		"message":  a.Body,
		"click":    a.Link,
		"tags":     []string{"rotating_light"},
		"priority": priority,
	}
	headers := map[string]string{}
	if m.cfg.ntfyToken != "" {
		headers["Authorization"] = "Bearer " + m.cfg.ntfyToken
	}
	return postJSON(ctx, m.client, m.cfg.ntfyURL, payload, headers)
}

func postJSON(ctx context.Context, client *http.Client, target string, payload any, headers map[string]string) error {
	body, err := json.Marshal(payload)
	if err != nil {
		return fmt.Errorf("encode payload: %w", err)
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, target, bytes.NewReader(body))
	if err != nil {
		return redactURL(err)
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("User-Agent", userAgent)
	for key, value := range headers {
		req.Header.Set(key, value)
	}
	resp, err := client.Do(req)
	if err != nil {
		return redactURL(err)
	}
	defer func() { _ = resp.Body.Close() }()
	_, _ = io.Copy(io.Discard, resp.Body)
	if resp.StatusCode >= http.StatusBadRequest {
		return fmt.Errorf("server answered %s", resp.Status)
	}
	return nil
}

func redactURL(err error) error {
	if urlErr, ok := errors.AsType[*url.Error](err); ok {
		return fmt.Errorf("%s request failed: %w", urlErr.Op, urlErr.Err)
	}
	return err
}

func (m *monitor) deliverPending(ctx context.Context) {
	for name, send := range m.channels() {
		m.deliverChannel(ctx, name, send)
	}
}

func (m *monitor) deliverChannel(ctx context.Context, name string, send sender) {
	for _, a := range m.st.Alerts {
		if slices.Contains(a.Delivered, name) {
			continue
		}
		if err := send(ctx, a); err != nil {
			slog.Warn("delivery failed, retrying next cycle", "channel", name, "err", err)
			return
		}
		m.mu.Lock()
		a.Delivered = append(a.Delivered, name)
		m.mu.Unlock()
	}
}
