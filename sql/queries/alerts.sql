-- name: InsertAlert :exec
INSERT INTO alerts (id, title, link, body, published_at)
VALUES (@id, @title, @link, @body, @published_at)
ON CONFLICT (id) DO NOTHING;

-- name: MarkDelivered :exec
INSERT INTO alert_deliveries (alert_id, channel) VALUES (@alert_id, @channel)
ON CONFLICT (alert_id, channel) DO NOTHING;

-- name: PendingAlerts :many
SELECT a.id, a.title, a.link, a.body, a.published_at
FROM alerts a
WHERE NOT EXISTS (
    SELECT 1 FROM alert_deliveries d WHERE d.alert_id = a.id AND d.channel = @channel
)
ORDER BY a.seq;

-- name: RecentAlerts :many
SELECT id, title, link, body, published_at
FROM alerts
ORDER BY seq DESC
LIMIT @max_alerts;

-- name: PruneAlerts :exec
DELETE FROM alerts
WHERE seq NOT IN (SELECT seq FROM alerts ORDER BY seq DESC LIMIT @keep);
