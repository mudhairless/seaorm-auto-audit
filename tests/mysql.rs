//! The audit trail against a real MySQL.
//!
//! These tests need a database. They run only when `MYSQL_DATABASE_URL` points
//! at one, and are skipped otherwise:
//!
//! ```console
//! $ docker run --rm -d --name seaorm-auto-audit-test -e MYSQL_ROOT_PASSWORD=password \
//!     -e MYSQL_DATABASE=test -p 3306:3306 mysql:8
//! $ MYSQL_DATABASE_URL=mysql://root:password@localhost:3306/test \
//!     cargo test -p seaorm-auto-audit --test mysql
//! ```

mod common;

use sea_orm::{
    ColumnTrait, ConnectionTrait, Database, DatabaseConnection, EntityTrait, QueryFilter,
};
use seaorm_auto_audit::Audits;
use tokio::sync::OnceCell;
use uuid::Uuid;

/// The DDL of the uuid-keyed audited table.
///
/// SeaORM binds a `uuid` to MySQL as its 16-byte form, so the column is
/// `BINARY(16)` rather than `CHAR(36)`.
const USERS_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS users (
    id BINARY(16) PRIMARY KEY,
    name TEXT NOT NULL,
    email TEXT,
    password_hash TEXT,
    age INTEGER NOT NULL
);
"#;

/// The DDL of the integer-keyed audited table.
const INTEGER_USERS_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS integer_users (
    id BIGINT PRIMARY KEY,
    name TEXT NOT NULL,
    email TEXT,
    password_hash TEXT,
    age INTEGER NOT NULL
);
"#;

/// The DDL of the string-keyed audited table.
const STRING_USERS_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS string_users (
    id VARCHAR(255) PRIMARY KEY,
    name TEXT NOT NULL,
    email TEXT,
    password_hash TEXT,
    age INTEGER NOT NULL
);
"#;

/// The DDL of the audit table, the same file the crate ships.
const AUDITS_DDL: &str = include_str!("../sql/setup.mysql.sql");

/// The schema is created once per test run.
static SETUP: OnceCell<()> = OnceCell::const_new();

/// A connection to the test database, with the schema in place.
///
/// Returns `None` when there is no `MYSQL_DATABASE_URL`, which skips the test.
async fn database() -> Option<DatabaseConnection> {
    let url = std::env::var("MYSQL_DATABASE_URL").ok()?;
    let db = Database::connect(&url)
        .await
        .expect("the test database should be reachable");

    SETUP
        .get_or_init(|| async {
            apply(&db, USERS_DDL).await;
            apply(&db, INTEGER_USERS_DDL).await;
            apply(&db, STRING_USERS_DDL).await;
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
async fn audits_of<D: std::fmt::Display>(
    db: &DatabaseConnection,
    id: &D,
) -> Vec<seaorm_auto_audit::audits::Model> {
    Audits::find()
        .filter(seaorm_auto_audit::audits::Column::AuditableId.eq(id.to_string()))
        .all(db)
        .await
        .expect("the audit rows should be readable")
}

common::real_database_suite!(uuid_keys, common::Entity, Uuid);
common::real_database_suite!(integer_keys, common::integer_key::Entity, i64);
common::real_database_suite!(string_keys, common::string_key::Entity, String);
