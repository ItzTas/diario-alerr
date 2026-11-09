-- name: CreateState :execrows
INSERT INTO monitor_state (cursor_at, last_success_at)
VALUES (@cursor_at, @last_success_at)
ON CONFLICT (singleton) DO NOTHING;

-- name: Cursor :one
SELECT cursor_at FROM monitor_state;

-- name: AdvanceCursor :exec
UPDATE monitor_state SET cursor_at = GREATEST(cursor_at, @cursor_at);

-- name: MarkSuccess :exec
UPDATE monitor_state SET last_success_at = @last_success_at;

-- name: LastSuccess :one
SELECT last_success_at FROM monitor_state;
