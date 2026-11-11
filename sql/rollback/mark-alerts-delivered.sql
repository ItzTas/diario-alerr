INSERT INTO alert_deliveries (alert_id, channel)
SELECT a.id, c.channel FROM alerts a CROSS JOIN (VALUES ('telegram'), ('ntfy')) AS c (channel)
ON CONFLICT (alert_id, channel) DO NOTHING;
