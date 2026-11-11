-- +goose Up
ALTER TABLE monitor_state ADD COLUMN acked_seq BIGINT NOT NULL DEFAULT 0;

-- +goose Down
ALTER TABLE monitor_state DROP COLUMN acked_seq;
