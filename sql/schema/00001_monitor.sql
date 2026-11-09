-- +goose Up
CREATE TABLE monitor_state (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    cursor_at TIMESTAMPTZ NOT NULL,
    last_success_at TIMESTAMPTZ
);

CREATE TABLE seen_editions (
    edition_id BIGINT PRIMARY KEY,
    seen_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE alerts (
    seq BIGSERIAL PRIMARY KEY,
    id TEXT NOT NULL UNIQUE,
    title TEXT NOT NULL,
    link TEXT NOT NULL,
    body TEXT NOT NULL,
    published_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE alert_deliveries (
    alert_id TEXT NOT NULL REFERENCES alerts (id) ON DELETE CASCADE,
    channel TEXT NOT NULL,
    delivered_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (alert_id, channel)
);

-- +goose Down
DROP TABLE alert_deliveries;
DROP TABLE alerts;
DROP TABLE seen_editions;
DROP TABLE monitor_state;
