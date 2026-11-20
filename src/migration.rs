mod goose;
mod m20261118_000001_goose_schema;
mod m20261118_000002_drop_go_tables;

use sea_orm_migration::{MigrationTrait, MigratorTrait};

pub const GOOSE_SCHEMA: &str = "m20261118_000001_goose_schema";
pub const DROP_GO_TABLES: &str = "m20261118_000002_drop_go_tables";

pub struct Migrator;

impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20261118_000001_goose_schema::Migration),
            Box::new(m20261118_000002_drop_go_tables::Migration),
        ]
    }
}
