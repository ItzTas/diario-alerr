mod support;

use diario_alerr::migration::{DROP_GO_TABLES, GOOSE_SCHEMA, Migrator};
use sea_orm_migration::MigratorTrait;
use tokio_postgres::{Client, NoTls};

use support::schema::{
    apply_up_sections, goose_era_database, rows, schema_snapshot, seed_rows, table_names,
};
use support::{database, orm};

const TODAYS_TABLES: [&str; 4] = [
    "alerts",
    "monitor_state",
    "seaql_migrations",
    "seen_editions",
];
const SEAQL_VERSIONS: &str = "SELECT version FROM seaql_migrations ORDER BY version";
const GOOSE_VERSIONS: &str = "SELECT version_id::text FROM goose_db_version ORDER BY id";
const SEQUENCE_STATE: &str = "SELECT format('%s %s', last_value, is_called) FROM alerts_seq_seq";

async fn admin(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect the admin client");
    tokio::spawn(connection);
    client
}

async fn texts(client: &Client, query: &str) -> Vec<String> {
    client
        .query(query, &[])
        .await
        .unwrap_or_else(|err| panic!("{query}: {err:?}"))
        .iter()
        .map(|row| row.get(0))
        .collect()
}

async fn kept_data(client: &Client) -> Vec<Vec<String>> {
    vec![
        rows(client, "alerts").await,
        rows(client, "monitor_state").await,
        rows(client, "seen_editions").await,
        texts(client, SEQUENCE_STATE).await,
    ]
}

fn with_migration_table(mut tables: Vec<String>) -> Vec<String> {
    tables.push("seaql_migrations".to_owned());
    tables.sort();
    tables
}

#[tokio::test]
async fn a_new_database_gets_todays_tables_and_no_go_tables() {
    let Some(db) = database().await else {
        return;
    };
    let client = admin(&db.url).await;

    Migrator::up(&orm(&db.url).await, None)
        .await
        .expect("migrate");

    assert_eq!(table_names(&client).await, TODAYS_TABLES);
    assert_eq!(
        texts(&client, SEAQL_VERSIONS).await,
        [GOOSE_SCHEMA, DROP_GO_TABLES]
    );
    let acked_seq = client
        .query_one(
            "SELECT data_type::text, is_nullable::text, column_default::text
            FROM information_schema.columns
            WHERE table_schema = current_schema()
              AND table_name = 'monitor_state' AND column_name = 'acked_seq'",
            &[],
        )
        .await
        .expect("describe acked_seq");
    let described: (String, String, Option<String>) =
        (acked_seq.get(0), acked_seq.get(1), acked_seq.get(2));
    assert_eq!(
        described,
        ("bigint".to_owned(), "NO".to_owned(), Some("0".to_owned()))
    );
}

#[tokio::test]
async fn the_first_migration_builds_what_the_goose_files_build() {
    let (Some(migrated), Some(by_hand)) = (database().await, database().await) else {
        return;
    };
    let migrated_client = admin(&migrated.url).await;
    let by_hand_client = admin(&by_hand.url).await;

    Migrator::up(&orm(&migrated.url).await, Some(1))
        .await
        .expect("apply the first migration");
    apply_up_sections(&by_hand_client, &[1, 2]).await;

    assert_eq!(
        schema_snapshot(&migrated_client, &["seaql_migrations"]).await,
        schema_snapshot(&by_hand_client, &[]).await
    );
}

#[tokio::test]
async fn a_database_at_goose_version_2_is_adopted_without_loss() {
    let Some(db) = database().await else {
        return;
    };
    let client = admin(&db.url).await;
    goose_era_database(&client, &[1, 2], &[1, 2]).await;
    seed_rows(&client, true).await;
    let before = kept_data(&client).await;

    Migrator::up(&orm(&db.url).await, None)
        .await
        .expect("migrate");

    assert_eq!(kept_data(&client).await, before);
    assert_eq!(table_names(&client).await, TODAYS_TABLES);
    assert_eq!(
        texts(&client, SEAQL_VERSIONS).await,
        [GOOSE_SCHEMA, DROP_GO_TABLES]
    );
}

#[tokio::test]
async fn a_database_at_goose_version_1_is_caught_up_and_ends_like_a_new_one() {
    let (Some(db), Some(new)) = (database().await, database().await) else {
        return;
    };
    let client = admin(&db.url).await;
    let new_client = admin(&new.url).await;
    goose_era_database(&client, &[1], &[1]).await;
    seed_rows(&client, false).await;
    let state = "SELECT format('%s %s', cursor_at, last_success_at) FROM monitor_state";
    let state_before = texts(&client, state).await;
    let alerts_before = rows(&client, "alerts").await;
    let editions_before = rows(&client, "seen_editions").await;
    let sequence_before = texts(&client, SEQUENCE_STATE).await;

    Migrator::up(&orm(&db.url).await, None)
        .await
        .expect("migrate");
    Migrator::up(&orm(&new.url).await, None)
        .await
        .expect("migrate the new database");

    assert_eq!(texts(&client, state).await, state_before);
    assert_eq!(
        texts(&client, "SELECT acked_seq::text FROM monitor_state").await,
        ["0"]
    );
    assert_eq!(rows(&client, "alerts").await, alerts_before);
    assert_eq!(rows(&client, "seen_editions").await, editions_before);
    assert_eq!(texts(&client, SEQUENCE_STATE).await, sequence_before);
    assert_eq!(
        schema_snapshot(&client, &[]).await,
        schema_snapshot(&new_client, &[]).await
    );
}

#[tokio::test]
async fn a_goose_version_above_2_is_adopted() {
    let Some(db) = database().await else {
        return;
    };
    let client = admin(&db.url).await;
    goose_era_database(&client, &[1, 2], &[1, 2, 3]).await;
    seed_rows(&client, true).await;
    let alerts_before = rows(&client, "alerts").await;

    Migrator::up(&orm(&db.url).await, None)
        .await
        .expect("migrate");

    assert_eq!(rows(&client, "alerts").await, alerts_before);
}

#[tokio::test]
async fn a_gap_in_goose_db_version_is_refused_and_nothing_is_dropped() {
    let Some(db) = database().await else {
        return;
    };
    let client = admin(&db.url).await;
    goose_era_database(&client, &[1, 2], &[2]).await;
    seed_rows(&client, true).await;
    let tables_before = table_names(&client).await;
    let goose_before = texts(&client, GOOSE_VERSIONS).await;
    let deliveries_before = rows(&client, "alert_deliveries").await;

    let refused = Migrator::up(&orm(&db.url).await, None)
        .await
        .expect_err("a gap in goose_db_version");

    assert!(
        refused.to_string().contains(
            "migration version 1 is not applied but the database is already at version 2"
        ),
        "{refused}"
    );
    // install creates seaql_migrations before the first migration, outside its transaction.
    assert_eq!(
        table_names(&client).await,
        with_migration_table(tables_before)
    );
    assert_eq!(
        texts(&client, "SELECT count(*)::text FROM seaql_migrations").await,
        ["0"]
    );
    assert_eq!(texts(&client, GOOSE_VERSIONS).await, goose_before);
    assert_eq!(rows(&client, "alert_deliveries").await, deliveries_before);
}

#[tokio::test]
async fn an_empty_goose_db_version_is_refused_and_nothing_is_dropped() {
    let Some(db) = database().await else {
        return;
    };
    let client = admin(&db.url).await;
    goose_era_database(&client, &[1, 2], &[]).await;
    client
        .batch_execute("DELETE FROM goose_db_version")
        .await
        .expect("empty goose_db_version");
    let tables_before = table_names(&client).await;

    let refused = Migrator::up(&orm(&db.url).await, None)
        .await
        .expect_err("an empty goose_db_version");

    assert!(
        refused
            .to_string()
            .contains("goose_db_version has no rows, not even the version 0 row goose expects"),
        "{refused}"
    );
    assert_eq!(
        table_names(&client).await,
        with_migration_table(tables_before)
    );
    assert_eq!(
        texts(&client, "SELECT count(*)::text FROM seaql_migrations").await,
        ["0"]
    );
}

#[tokio::test]
async fn a_failed_catch_up_applies_nothing_and_drops_nothing() {
    let Some(db) = database().await else {
        return;
    };
    let client = admin(&db.url).await;
    goose_era_database(&client, &[1], &[1]).await;
    seed_rows(&client, false).await;
    client
        .batch_execute("ALTER TABLE monitor_state ADD COLUMN acked_seq BIGINT NOT NULL DEFAULT 0")
        .await
        .expect("add acked_seq by hand");

    let failed = Migrator::up(&orm(&db.url).await, None)
        .await
        .expect_err("version 2 cannot run twice");

    assert!(
        failed
            .to_string()
            .contains(r#"column "acked_seq" of relation "monitor_state" already exists"#),
        "{failed}"
    );
    assert_eq!(texts(&client, GOOSE_VERSIONS).await, ["0", "1"]);
    assert!(
        table_names(&client)
            .await
            .contains(&"alert_deliveries".to_owned())
    );
    assert_eq!(
        texts(&client, "SELECT count(*)::text FROM seaql_migrations").await,
        ["0"]
    );
}

#[tokio::test]
async fn a_new_database_whose_catch_up_fails_keeps_none_of_it() {
    let Some(db) = database().await else {
        return;
    };
    let client = admin(&db.url).await;
    // Refuses 00002's ALTER TABLE after 00001 has created its tables, so only the first
    // migration's transaction can take them away again.
    client
        .batch_execute(
            "CREATE FUNCTION refuse_alter_table() RETURNS event_trigger LANGUAGE plpgsql AS $$
            BEGIN
                RAISE EXCEPTION 'ALTER TABLE refused by the test';
            END
            $$;
            CREATE EVENT TRIGGER refuse_alter_table ON ddl_command_start
                WHEN TAG IN ('ALTER TABLE') EXECUTE FUNCTION refuse_alter_table();",
        )
        .await
        .expect("create the event trigger");

    let failed = Migrator::up(&orm(&db.url).await, None)
        .await
        .expect_err("the event trigger refuses 00002");

    assert!(
        failed
            .to_string()
            .contains("ALTER TABLE refused by the test"),
        "{failed}"
    );
    assert_eq!(table_names(&client).await, ["seaql_migrations"]);
    assert_eq!(
        texts(&client, "SELECT count(*)::text FROM seaql_migrations").await,
        ["0"]
    );
}

async fn a_view_stops_the_drop(table: &str, view: &str) {
    let Some(db) = database().await else {
        return;
    };
    let client = admin(&db.url).await;
    goose_era_database(&client, &[1, 2], &[1, 2]).await;
    seed_rows(&client, true).await;
    client
        .batch_execute(&format!("CREATE VIEW {view} AS SELECT * FROM {table}"))
        .await
        .expect("create the view");
    let alerts_before = rows(&client, "alerts").await;
    let deliveries_before = rows(&client, "alert_deliveries").await;
    let goose_before = texts(&client, GOOSE_VERSIONS).await;

    let failed = Migrator::up(&orm(&db.url).await, None)
        .await
        .expect_err("the view stops the drop");

    let refusal = format!("cannot drop table {table} because other objects depend on it");
    assert!(failed.to_string().contains(&refusal), "{failed}");
    assert_eq!(texts(&client, SEAQL_VERSIONS).await, [GOOSE_SCHEMA]);
    let tables = table_names(&client).await;
    assert!(
        tables.contains(&"alert_deliveries".to_owned()),
        "{tables:?}"
    );
    assert!(
        tables.contains(&"goose_db_version".to_owned()),
        "{tables:?}"
    );
    assert_eq!(
        texts(
            &client,
            "SELECT viewname::text FROM pg_views WHERE schemaname = current_schema()"
        )
        .await,
        [view]
    );
    assert_eq!(rows(&client, "alerts").await, alerts_before);
    assert_eq!(rows(&client, "alert_deliveries").await, deliveries_before);
    assert_eq!(texts(&client, GOOSE_VERSIONS).await, goose_before);
}

#[tokio::test]
async fn an_object_that_depends_on_a_go_table_stops_the_drop() {
    a_view_stops_the_drop("alert_deliveries", "delivered").await;
}

#[tokio::test]
async fn an_object_that_depends_on_goose_db_version_keeps_alert_deliveries_too() {
    a_view_stops_the_drop("goose_db_version", "goose_versions").await;
}

#[tokio::test]
async fn running_the_migrations_again_changes_nothing() {
    let Some(db) = database().await else {
        return;
    };
    let client = admin(&db.url).await;
    let orm = orm(&db.url).await;
    let recorded =
        "SELECT format('%s %s', version, applied_at) FROM seaql_migrations ORDER BY version";
    Migrator::up(&orm, None).await.expect("migrate");
    let before = texts(&client, recorded).await;

    Migrator::up(&orm, None).await.expect("migrate again");

    assert!(
        Migrator::get_pending_migrations(&orm)
            .await
            .expect("pending migrations")
            .is_empty()
    );
    assert_eq!(texts(&client, recorded).await, before);
}

#[tokio::test]
async fn go_tables_in_another_schema_on_the_search_path_are_left_alone() {
    let Some(db) = database().await else {
        return;
    };
    let setup = admin(&db.url).await;
    goose_era_database(&setup, &[], &[1, 2]).await;
    setup
        .batch_execute(
            "CREATE TABLE alert_deliveries (alert_id TEXT, channel TEXT);
            INSERT INTO alert_deliveries VALUES ('another-application', 'email');
            CREATE SCHEMA diario;",
        )
        .await
        .expect("set up another application's tables in public");
    let name = texts(&setup, "SELECT current_database()::text").await;
    setup
        .batch_execute(&format!(
            "ALTER DATABASE {} SET search_path = diario, public",
            name[0]
        ))
        .await
        .expect("put diario first on the search path");
    let client = admin(&db.url).await;
    let public_tables =
        "SELECT tablename::text FROM pg_tables WHERE schemaname = 'public' ORDER BY 1";
    let goose_before = rows(&client, "public.goose_db_version").await;
    let deliveries_before = rows(&client, "public.alert_deliveries").await;

    Migrator::up(&orm(&db.url).await, None)
        .await
        .expect("migrate");

    assert_eq!(
        texts(&client, "SELECT current_schema()::text").await,
        ["diario"]
    );
    assert_eq!(table_names(&client).await, TODAYS_TABLES);
    assert_eq!(
        texts(&client, public_tables).await,
        ["alert_deliveries", "goose_db_version"]
    );
    assert_eq!(rows(&client, "public.goose_db_version").await, goose_before);
    assert_eq!(
        rows(&client, "public.alert_deliveries").await,
        deliveries_before
    );
}
