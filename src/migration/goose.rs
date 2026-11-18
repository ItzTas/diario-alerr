use std::collections::HashSet;

use sea_orm::{ConnectionTrait, DbErr, Statement};

pub(crate) const GOOSE_FILES: [(i64, &str); 2] = [
    (1, include_str!("../../sql/schema/00001_monitor.sql")),
    (2, include_str!("../../sql/schema/00002_acked_seq.sql")),
];

const TABLE_EXISTS: &str = "SELECT EXISTS ( SELECT 1 FROM pg_tables WHERE (current_schema() IS NULL OR schemaname = current_schema()) AND tablename = 'goose_db_version' )";
const LIST_VERSIONS: &str = "SELECT version_id FROM goose_db_version ORDER BY id DESC";

// The texts are those of the SDK's goose-compatible runner, which 2.1 migrated with.
#[derive(Debug, thiserror::Error)]
pub(crate) enum GooseError {
    #[error(
        "migration version {missing} is not applied but the database is already at version {current}"
    )]
    OutOfOrder { missing: i64, current: i64 },
    #[error("goose_db_version has no rows, not even the version 0 row goose expects")]
    MissingZeroVersion,
    #[error(transparent)]
    Database(#[from] DbErr),
}

pub(crate) fn up_section(file: &str) -> &str {
    file.split_once("-- +goose Down").map_or(file, |(up, _)| up)
}

/// Runs the goose files the database has not recorded, as goose would, and records nothing in
/// `goose_db_version`. A database without that table is new and gets every file.
pub(crate) async fn catch_up(db: &impl ConnectionTrait) -> Result<Vec<i64>, GooseError> {
    let recorded = recorded_versions(db).await?;
    let current = recorded.iter().copied().max().unwrap_or(0);
    let pending: Vec<(i64, &str)> = GOOSE_FILES
        .into_iter()
        .filter(|(version, _)| !recorded.contains(version))
        .collect();
    if let Some(&(missing, _)) = pending.iter().find(|(version, _)| *version < current) {
        return Err(GooseError::OutOfOrder { missing, current });
    }
    for (_, file) in &pending {
        db.execute_unprepared(up_section(file)).await?;
    }
    Ok(pending.into_iter().map(|(version, _)| version).collect())
}

async fn recorded_versions(db: &impl ConnectionTrait) -> Result<HashSet<i64>, GooseError> {
    let backend = db.get_database_backend();
    let exists = db
        .query_one_raw(Statement::from_string(backend, TABLE_EXISTS))
        .await?
        .ok_or_else(|| DbErr::Custom("the goose_db_version check returned no row".to_owned()))?
        .try_get_by_index::<bool>(0)?;
    if !exists {
        return Ok(HashSet::new());
    }
    let rows = db
        .query_all_raw(Statement::from_string(backend, LIST_VERSIONS))
        .await?;
    if rows.is_empty() {
        return Err(GooseError::MissingZeroVersion);
    }
    rows.iter()
        .map(|row| Ok(row.try_get_by_index::<i64>(0)?))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn up_sections_stop_before_the_down_section() {
        let [(1, first), (2, second)] = GOOSE_FILES else {
            panic!("the goose files are versions 1 and 2");
        };
        assert!(up_section(first).contains("CREATE TABLE alerts"));
        assert!(!up_section(first).contains("DROP TABLE"));
        assert!(up_section(second).contains("ADD COLUMN acked_seq"));
        assert!(!up_section(second).contains("DROP COLUMN"));
    }
}
