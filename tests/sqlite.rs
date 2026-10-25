//! The audit trail against a real SQLite.
//!
//! These tests always run: without `SQLITE_DATABASE_URL` they use a temporary
//! file, so the suite does not need a database server.

mod common;

use std::sync::OnceLock;

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
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    email TEXT,
    password_hash TEXT,
    age INTEGER NOT NULL
);
"#;

/// The DDL of the audit table, the same file the crate ships.
const AUDITS_DDL: &str = include_str!("../sql/setup.sqlite.sql");

/// The schema is created once per test run.
static SETUP: OnceCell<()> = OnceCell::const_new();

/// A file-backed temporary database.
///
/// Every connection to `sqlite::memory:` is a separate database, so the
/// schema created on the first connection would be invisible to the rest.
/// A temp file keeps every connection on the same database and is removed
/// when the process ends.
static TEMP: OnceLock<tempfile::NamedTempFile> = OnceLock::new();

fn database_url() -> String {
    if let Ok(url) = std::env::var("SQLITE_DATABASE_URL") {
        return url;
    }
    let file = TEMP.get_or_init(|| {
        tempfile::Builder::new()
            .prefix("seaorm-auto-audit-")
            .suffix(".sqlite")
            .tempfile()
            .expect("the temporary database should be creatable")
    });
    format!("sqlite://{}", file.path().display())
}

/// A connection to the test database, with the schema in place.
async fn database() -> Option<DatabaseConnection> {
    let url = database_url();
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
