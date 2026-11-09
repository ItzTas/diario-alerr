-- name: MarkEditionSeen :exec
INSERT INTO seen_editions (edition_id) VALUES (@edition_id)
ON CONFLICT (edition_id) DO NOTHING;

-- name: SeenEditions :many
SELECT edition_id FROM seen_editions WHERE edition_id = ANY(@edition_ids::BIGINT[]);

-- name: PruneSeenEditions :exec
DELETE FROM seen_editions
WHERE edition_id NOT IN (
    SELECT edition_id FROM seen_editions ORDER BY edition_id DESC LIMIT @keep
);
