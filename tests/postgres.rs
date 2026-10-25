//! The audit trail against a real Postgres.
//!
//! These tests need a database. They run only when `DATABASE_URL` points at
//! one, and are skipped otherwise:
//!
//! ```console
//! $ docker run --rm -d --name seaorm-auto-audit-test -e POSTGRES_PASSWORD=postgres \
//!     -p 5432:5432 postgres:18-alpine
//! $ DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres \
//!     cargo test -p seaorm-auto-audit --test postgres
//! ```

mod common;

use common::ActiveModel;
use sea_orm::{
    ActiveModelTrait,
    ActiveValue::{NotSet, Set, Unchanged},
    ColumnTrait, Condition, ConnectionTrait, Database, DatabaseConnection, EntityTrait,
    QueryFilter, TransactionTrait,
};
use seaorm_auto_audit::{
    Audits, RequestMetadata, delete_by_id as audited_delete_by_id,
    delete_many as audited_delete_many, insert_many as audited_insert_many, scope,
    scope_with_request, update_many as audited_update_many,
};
use serde_json::json;
use tokio::sync::OnceCell;
use uuid::Uuid;

/// The DDL of the audited table.
const USERS_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS users (
    id uuid PRIMARY KEY,
    name text NOT NULL,
    email text,
    password_hash text,
    age integer NOT NULL
);
"#;

/// The DDL of the audit table, the same file the crate ships.
const AUDITS_DDL: &str = include_str!("../sql/setup.postgres.sql");

/// The schema is created once per test run.
static SETUP: OnceCell<()> = OnceCell::const_new();

/// A connection to the test database, with the schema in place.
///
/// Returns `None` when there is no `DATABASE_URL`, which skips the test.
async fn database() -> Option<DatabaseConnection> {
    let url = std::env::var("DATABASE_URL").ok()?;
    let db = Database::connect(&url)
        .await
        .expect("the test database should be reachable");

    SETUP
        .get_or_init(|| async {
            apply(&db, USERS_DDL).await;
            apply(&db, AUDITS_DDL).await;
        })
        .await;

    Some(db)
}

/// Run a DDL script, statement by statement.
async fn apply(db: &DatabaseConnection, ddl: &str) {
    let script = ddl
        .lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n");

    for statement in script.split(';') {
        let statement = statement.trim();
        if statement.is_empty() {
            continue;
        }
        db.execute_unprepared(statement)
            .await
            .unwrap_or_else(|error| panic!("`{statement}` failed: {error}"));
    }
}

/// The audit rows of one row of the audited table.
async fn audits_of(db: &DatabaseConnection, id: Uuid) -> Vec<seaorm_auto_audit::audits::Model> {
    Audits::find()
        .filter(seaorm_auto_audit::audits::Column::AuditableId.eq(id.to_string()))
        .all(db)
        .await
        .expect("the audit rows should be readable")
}

include!("common/real_database_suite.rs");
