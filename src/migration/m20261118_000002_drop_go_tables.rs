use sea_orm::{ConnectionTrait, DbErr, DeriveMigrationName, Statement};
use sea_orm_migration::async_trait::async_trait;
use sea_orm_migration::{MigrationTrait, SchemaManager};

const CURRENT_SCHEMA: &str = "SELECT quote_ident(current_schema())";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        let schema: String = db
            .query_one_raw(Statement::from_string(
                db.get_database_backend(),
                CURRENT_SCHEMA,
            ))
            .await?
            .ok_or_else(|| DbErr::Custom("the current schema query returned no row".to_owned()))?
            .try_get_by_index(0)?;
        // Qualified with the schema the adoption read goose_db_version from: unqualified, a table
        // missing there would resolve through search_path to another schema's table.
        // No CASCADE: an object that depends on one of these tables makes its DROP fail, the
        // migration's transaction undoes both, and start-up stops with Postgres's error. One
        // statement per table, so that error names the table.
        db.execute_unprepared(&format!(
            "DROP TABLE IF EXISTS {schema}.alert_deliveries;\n\
             DROP TABLE IF EXISTS {schema}.goose_db_version;"
        ))
        .await?;
        Ok(())
    }
}
