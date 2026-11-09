package health

import (
	"log/slog"
	"net/http"

	"github.com/jackc/pgx/v5/pgxpool"
)

func Configure(mux *http.ServeMux, pool *pgxpool.Pool, opts Options, log *slog.Logger) {
	RegisterRoutes(mux, &Handler{service: NewService(NewPostgres(pool), opts), log: log})
}

func RegisterRoutes(mux *http.ServeMux, h *Handler) {
	mux.HandleFunc("GET /healthz", h.Health)
}
