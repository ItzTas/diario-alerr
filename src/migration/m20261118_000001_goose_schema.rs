use sea_orm::{DbErr, DeriveMigrationName};
use sea_orm_migration::async_trait::async_trait;
use sea_orm_migration::{MigrationTrait, SchemaManager};

use super::goose::{GooseError, catch_up};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        catch_up(manager.get_connection())
            .await
            .map_err(|err| match err {
                GooseError::Database(err) => err,
                refused => DbErr::Migration(refused.to_string()),
            })
    }
}
