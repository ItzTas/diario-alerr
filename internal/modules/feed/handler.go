package feed

import (
	"log/slog"
	"net/http"
)

type Handler struct {
	service *Service
	log     *slog.Logger
}

func (h *Handler) Feed(w http.ResponseWriter, r *http.Request) {
	body, err := h.service.Render(r.Context())
	if err != nil {
		h.log.Error("could not render the feed", "err", err)
		http.Error(w, "could not render feed", http.StatusInternalServerError)
		return
	}
	w.Header().Set("Content-Type", "application/rss+xml; charset=utf-8")
	_, _ = w.Write(body)
}
