//! The audit trail against a real SQLite.
//!
//! These tests need a database. They run only when `SQLITE_DATABASE_URL` points at
//! one or use a file path, and are skipped otherwise.

mod common;

use common::ActiveModel;
use seaorm_auto_audit::{
    Audits, RequestMetadata, delete_by_id as audited_delete_by_id,
    delete_many as audited_delete_many, insert_many as audited_insert_many, scope,
    scope_with_request, update_many as audited_update_many,
};
use sea_orm::{
    ActiveModelTrait,
    ActiveValue::{NotSet, Set, Unchanged},
    ColumnTrait, Condition, ConnectionTrait, Database, DatabaseConnection, EntityTrait,
    QueryFilter, TransactionTrait,
};
use serde_json::json;
use tokio::sync::OnceCell;
use uuid::Uuid;

const USERS_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS users (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    email TEXT,
    password_hash TEXT,
    age INTEGER NOT NULL
);
"#;

const AUDITS_DDL: &str = include_str!("../sql/setup.sqlite.sql");

static SETUP: OnceCell<()> = OnceCell::const_new();

async fn database() -> Option<DatabaseConnection> {
    let url = std::env::var("SQLITE_DATABASE_URL").unwrap_or_else(|_| "sqlite::memory:".to_string());
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

async fn audits_of(db: &DatabaseConnection, id: Uuid) -> Vec<seaorm_auto_audit::audits::Model> {
    Audits::find()
        .filter(seaorm_auto_audit::audits::Column::AuditableId.eq(id.to_string()))
        .all(db)
        .await
        .expect("the audit rows should be readable")
}

#[tokio::test]
async fn a_write_is_audited_in_a_real_database() {
    let Some(db) = database().await else {
        return;
    };
    let id = Uuid::new_v4();
    let actor = Uuid::new_v4();

    let txn = db.begin().await.expect("the transaction should start");
    scope(Some(actor), async {
        ActiveModel {
            id: Set(id.to_string()),
            name: Set("Ada".to_owned()),
            email: Set(Some("ada@example.com".to_owned())),
            password_hash: Set(Some("hash-before".to_owned())),
            age: Set(36),
        }
        .insert(&txn)
        .await
        .expect("the insert should succeed");
    })
    .await;
    txn.commit().await.expect("the transaction should commit");

    let audits = audits_of(&db, id).await;
    assert_eq!(audits.len(), 1, "one audit row per write");

    let audit = &audits[0];
    assert_eq!(audit.auditable_table, "users");
    assert_eq!(audit.auditable_id.to_string(), id.to_string());
    assert_eq!(audit.action, "create");
    assert_eq!(audit.updated_by_id.as_ref().map(|v| v.to_string()), Some(actor.to_string()));
    assert!(audit.before_content.is_none(), "a create has no before");

    let after = audit.after_content.as_ref().expect("an after");
    assert_eq!(after["name"], json!("Ada"));
    assert!(
        after.get("password_hash").is_none(),
        "the credential is never stored: {after}"
    );
}
