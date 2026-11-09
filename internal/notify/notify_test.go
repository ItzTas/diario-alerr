package notify

import (
	"encoding/json"
	"maps"
	"net/http"
	"net/http/httptest"
	"reflect"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/ItzTas/diario-alerr/internal/core"
)

func TestChannelsFollowConfiguration(t *testing.T) {
	cases := map[string]struct {
		cfg  core.Config
		want []string
	}{
		"none":     {core.Config{NtfyTopic: "x"}, []string{}},
		"telegram": {core.Config{TelegramToken: "t", TelegramChatID: "c"}, []string{"telegram"}},
		"ntfy":     {core.Config{NtfyURL: "http://n", NtfyTopic: "x"}, []string{"ntfy"}},
		"both":     {core.Config{TelegramToken: "t", TelegramChatID: "c", NtfyURL: "http://n", NtfyTopic: "x"}, []string{"ntfy", "telegram"}},
	}
	for name, tc := range cases {
		got := slices.Sorted(maps.Keys(Channels(tc.cfg, http.DefaultClient)))
		if got == nil {
			got = []string{}
		}
		if !reflect.DeepEqual(got, tc.want) {
			t.Errorf("%s: channels = %v, want %v", name, got, tc.want)
		}
	}
}

func TestNtfySendsTheAlertWithItsPriority(t *testing.T) {
	var bodies []map[string]any
	var auth []string
	server := httptest.NewServer(http.HandlerFunc(func(_ http.ResponseWriter, r *http.Request) {
		var body map[string]any
		_ = json.NewDecoder(r.Body).Decode(&body)
		bodies = append(bodies, body)
		auth = append(auth, r.Header.Get("Authorization"))
	}))
	defer server.Close()
	ntfy := Ntfy{Client: server.Client(), URL: server.URL, Topic: "diario-alerr", Token: "tk_test"}

	for _, id := range []string{"edition-1", core.WelcomeAlertID} {
		if err := ntfy.Send(t.Context(), core.Alert{ID: id, Title: "T", Link: "L", Body: "B"}); err != nil {
			t.Fatal(err)
		}
	}
	want := []map[string]any{
		{"topic": "diario-alerr", "title": "T", "message": "B", "click": "L", "tags": []any{"rotating_light"}, "priority": float64(5)},
		{"topic": "diario-alerr", "title": "T", "message": "B", "click": "L", "tags": []any{"rotating_light"}, "priority": float64(3)},
	}
	if !reflect.DeepEqual(bodies, want) {
		t.Errorf("payloads = %v\nwant       %v", bodies, want)
	}
	if !reflect.DeepEqual(auth, []string{"Bearer tk_test", "Bearer tk_test"}) {
		t.Errorf("Authorization = %v", auth)
	}
}

func TestNtfyReportsAServerError(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusInternalServerError)
	}))
	defer server.Close()
	ntfy := Ntfy{Client: server.Client(), URL: server.URL, Topic: "x"}
	if err := ntfy.Send(t.Context(), core.Alert{ID: "a"}); err == nil || !strings.Contains(err.Error(), "500") {
		t.Errorf("err = %v, want the 500 status", err)
	}
}

func TestTelegramPostsAnHTMLMessageToTheBot(t *testing.T) {
	var path string
	var body map[string]any
	server := httptest.NewServer(http.HandlerFunc(func(_ http.ResponseWriter, r *http.Request) {
		path = r.URL.Path
		_ = json.NewDecoder(r.Body).Decode(&body)
	}))
	defer server.Close()
	telegram := Telegram{Client: server.Client(), APIURL: server.URL, Token: "123:SECRET", ChatID: "42"}
	alert := core.Alert{ID: "edition-1", Title: "Found <you>", Link: "https://x/a.pdf#page=2", Body: "A & B", Published: time.Now()}
	if err := telegram.Send(t.Context(), alert); err != nil {
		t.Fatal(err)
	}
	if path != "/bot123:SECRET/sendMessage" {
		t.Errorf("path = %q", path)
	}
	wantText := "🚨 <b>Found &lt;you&gt;</b>\n\n<pre>A &amp; B</pre>\n<a href=\"https://x/a.pdf#page=2\">Open the PDF at the page</a>"
	if body["chat_id"] != "42" || body["parse_mode"] != "HTML" || body["text"] != wantText {
		t.Errorf("body = %v", body)
	}
}

func TestTelegramErrorOmitsTheBotToken(t *testing.T) {
	telegram := Telegram{Client: http.DefaultClient, APIURL: "http://127.0.0.1:1", Token: "123:SECRET", ChatID: "42"}
	err := telegram.Send(t.Context(), core.Alert{ID: "x"})
	if err == nil {
		t.Fatal("Send to a closed port returned nil")
	}
	if strings.Contains(err.Error(), "SECRET") {
		t.Errorf("error leaks the token: %v", err)
	}
}
