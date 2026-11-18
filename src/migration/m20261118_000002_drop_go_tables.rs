use sea_orm::{ConnectionTrait, DbErr, DeriveMigrationName};
use sea_orm_migration::async_trait::async_trait;
use sea_orm_migration::{MigrationTrait, SchemaManager};

// No CASCADE: an object that depends on one of these tables makes its DROP fail, the migration's
// transaction undoes both, and start-up stops with Postgres's error. One statement per table, so
// that error names the table.
const DROP: &str = "DROP TABLE IF EXISTS alert_deliveries;
DROP TABLE IF EXISTS goose_db_version;";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(DROP).await?;
        Ok(())
    }
}
