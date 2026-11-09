package health

import (
	"log/slog"
	"net/http"
)

type Handler struct {
	service *Service
	log     *slog.Logger
}

func (h *Handler) Health(w http.ResponseWriter, r *http.Request) {
	healthy, err := h.service.Healthy(r.Context())
	if err != nil {
		h.log.Error("could not read the last successful check", "err", err)
	}
	if healthy {
		_, _ = w.Write([]byte("ok"))
		return
	}
	http.Error(w, "stale", http.StatusServiceUnavailable)
}
