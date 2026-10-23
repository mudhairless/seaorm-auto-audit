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
const AUDITS_DDL: &str = include_str!("../sql/audits.sql");

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
        .filter(seaorm_auto_audit::audits::Column::AuditableId.eq(id))
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
            id: Set(id),
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
    assert_eq!(audit.auditable_id, id);
    assert_eq!(audit.action, "create");
    assert_eq!(audit.updated_by_id, Some(actor));
    assert!(audit.before_content.is_none(), "a create has no before");

    let after = audit.after_content.as_ref().expect("an after");
    assert_eq!(after["name"], json!("Ada"));
    assert!(
        after.get("password_hash").is_none(),
        "the credential is never stored: {after}"
    );
}

#[tokio::test]
async fn an_update_records_the_before_state() {
    let Some(db) = database().await else {
        return;
    };
    let id = Uuid::new_v4();
    let actor = Uuid::new_v4();

    let txn = db.begin().await.expect("the transaction should start");
    scope(Some(actor), async {
        ActiveModel {
            id: Set(id),
            name: Set("Ada".to_owned()),
            email: Set(None),
            password_hash: Set(Some("hash-before".to_owned())),
            age: Set(36),
        }
        .insert(&txn)
        .await
        .expect("the insert should succeed");

        ActiveModel {
            id: Unchanged(id),
            name: Set("Grace".to_owned()),
            email: NotSet,
            password_hash: NotSet,
            age: Set(37),
        }
        .update(&txn)
        .await
        .expect("the update should succeed");
    })
    .await;
    txn.commit().await.expect("the transaction should commit");

    let audits = audits_of(&db, id).await;
    assert_eq!(audits.len(), 2, "one audit row per write");

    let update = audits
        .iter()
        .find(|audit| audit.action == "update")
        .expect("an audit row for the update");

    let before = update.before_content.as_ref().expect("a before");
    let after = update.after_content.as_ref().expect("an after");
    assert_eq!(before["name"], json!("Ada"));
    assert_eq!(after["name"], json!("Grace"));
    assert_eq!(before["age"], json!(36));
    assert_eq!(after["age"], json!(37));
}

#[tokio::test]
async fn a_credential_change_is_marked_but_never_stored() {
    let Some(db) = database().await else {
        return;
    };
    let id = Uuid::new_v4();
    let actor = Uuid::new_v4();

    let txn = db.begin().await.expect("the transaction should start");
    scope(Some(actor), async {
        ActiveModel {
            id: Set(id),
            name: Set("Ada".to_owned()),
            email: Set(None),
            password_hash: Set(Some("hash-before".to_owned())),
            age: Set(36),
        }
        .insert(&txn)
        .await
        .expect("the insert should succeed");

        ActiveModel {
            id: Unchanged(id),
            name: NotSet,
            email: NotSet,
            password_hash: Set(Some("hash-after".to_owned())),
            age: NotSet,
        }
        .update(&txn)
        .await
        .expect("the update should succeed");
    })
    .await;
    txn.commit().await.expect("the transaction should commit");

    let audits = audits_of(&db, id).await;
    let update = audits
        .iter()
        .find(|audit| audit.action == "update")
        .expect("an audit row for the update");

    let before = update.before_content.as_ref().expect("a before");
    let after = update.after_content.as_ref().expect("an after");
    assert_eq!(before["password_hash"], json!(seaorm_auto_audit::REDACTED));
    assert_eq!(after["password_hash"], json!(seaorm_auto_audit::REDACTED));

    let row = format!("{before}{after}");
    assert!(!row.contains("hash-before"), "the old hash leaked: {row}");
    assert!(!row.contains("hash-after"), "the new hash leaked: {row}");
}

#[tokio::test]
async fn a_rolled_back_write_leaves_no_audit_row() {
    let Some(db) = database().await else {
        return;
    };
    let id = Uuid::new_v4();
    let actor = Uuid::new_v4();

    let txn = db.begin().await.expect("the transaction should start");
    scope(Some(actor), async {
        ActiveModel {
            id: Set(id),
            name: Set("Ada".to_owned()),
            email: Set(None),
            password_hash: Set(None),
            age: Set(36),
        }
        .insert(&txn)
        .await
        .expect("the insert should succeed");
    })
    .await;
    txn.rollback()
        .await
        .expect("the transaction should roll back");

    // The write and its audit row rolled back together.
    let user = common::Entity::find_by_id(id)
        .one(&db)
        .await
        .expect("the row should be readable");
    assert!(user.is_none(), "the row was rolled back");

    let audits = audits_of(&db, id).await;
    assert!(audits.is_empty(), "the audit row was rolled back too");
}

#[tokio::test]
async fn a_machine_made_change_is_audited_without_an_actor() {
    let Some(db) = database().await else {
        return;
    };
    let id = Uuid::new_v4();

    let txn = db.begin().await.expect("the transaction should start");
    scope(None, async {
        ActiveModel {
            id: Set(id),
            name: Set("Ada".to_owned()),
            email: Set(None),
            password_hash: Set(None),
            age: Set(36),
        }
        .insert(&txn)
        .await
        .expect("the insert should succeed");
    })
    .await;
    txn.commit().await.expect("the transaction should commit");

    let audits = audits_of(&db, id).await;
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].updated_by_id, None, "nobody was behind it");
    assert_eq!(audits[0].action, "create");
}

#[tokio::test]
async fn a_delete_is_audited() {
    let Some(db) = database().await else {
        return;
    };
    let id = Uuid::new_v4();
    let actor = Uuid::new_v4();

    let txn = db.begin().await.expect("the transaction should start");
    scope(Some(actor), async {
        ActiveModel {
            id: Set(id),
            name: Set("Ada".to_owned()),
            email: Set(None),
            password_hash: Set(Some("hash-before".to_owned())),
            age: Set(36),
        }
        .insert(&txn)
        .await
        .expect("the insert should succeed");

        ActiveModel {
            id: Unchanged(id),
            name: NotSet,
            email: NotSet,
            password_hash: NotSet,
            age: NotSet,
        }
        .delete(&txn)
        .await
        .expect("the delete should succeed");
    })
    .await;
    txn.commit().await.expect("the transaction should commit");

    let audits = audits_of(&db, id).await;
    let delete = audits
        .iter()
        .find(|audit| audit.action == "delete")
        .expect("an audit row for the delete");

    assert_eq!(delete.updated_by_id, Some(actor));
    assert!(delete.after_content.is_none(), "a delete has no after");

    let before = delete.before_content.as_ref().expect("a before");
    assert_eq!(before["name"], json!("Ada"));
    assert!(before.get("password_hash").is_none());
}

/// Two rows of the audited table, in the shape `insert_many` takes.
fn users(ids: [Uuid; 2]) -> Vec<ActiveModel> {
    ids.into_iter()
        .map(|id| ActiveModel {
            id: Set(id),
            name: Set("Ada".to_owned()),
            email: Set(None),
            password_hash: Set(Some("hash-before".to_owned())),
            age: Set(36),
        })
        .collect()
}

/// The request details a middleware would have captured.
fn request_metadata() -> RequestMetadata {
    RequestMetadata {
        ip_address: Some("203.0.113.7".to_owned()),
        url: Some("http://api.example.com/users".to_owned()),
        user_agent: Some("ftg-test/1.0".to_owned()),
    }
}

#[tokio::test]
async fn insert_many_audits_every_row_and_records_the_request() {
    let Some(db) = database().await else {
        return;
    };
    let ids = [Uuid::new_v4(), Uuid::new_v4()];
    let actor = Uuid::new_v4();

    let txn = db.begin().await.expect("the transaction should start");
    scope_with_request(Some(actor), Some(request_metadata()), async {
        audited_insert_many::<common::Entity, _, _>(&txn, users(ids))
            .await
            .expect("the bulk insert should succeed");
    })
    .await;
    txn.commit().await.expect("the transaction should commit");

    for id in ids {
        let audits = audits_of(&db, id).await;
        assert_eq!(audits.len(), 1, "one audit row per inserted row");

        let audit = &audits[0];
        assert_eq!(audit.action, "create");
        assert_eq!(audit.updated_by_id, Some(actor));
        assert_eq!(audit.ip_address.as_deref(), Some("203.0.113.7"));
        assert_eq!(audit.url.as_deref(), Some("http://api.example.com/users"));
        assert_eq!(audit.user_agent.as_deref(), Some("ftg-test/1.0"));

        let after = audit.after_content.as_ref().expect("an after");
        assert_eq!(after["name"], json!("Ada"));
        assert!(
            after.get("password_hash").is_none(),
            "the credential is never stored: {after}"
        );
    }
}

#[tokio::test]
async fn update_many_audits_every_row_it_changed() {
    let Some(db) = database().await else {
        return;
    };
    let ids = [Uuid::new_v4(), Uuid::new_v4()];
    let actor = Uuid::new_v4();

    let txn = db.begin().await.expect("the transaction should start");
    scope(Some(actor), async {
        audited_insert_many::<common::Entity, _, _>(&txn, users(ids))
            .await
            .expect("the seed insert should succeed");

        let filter = Condition::all().add(common::Column::Id.is_in(ids));
        let values = ActiveModel {
            id: NotSet,
            name: Set("Grace".to_owned()),
            email: NotSet,
            password_hash: Set(Some("hash-after".to_owned())),
            age: Set(37),
        };
        let result = audited_update_many::<common::Entity, _>(&txn, filter, values)
            .await
            .expect("the bulk update should succeed");
        assert_eq!(result.rows_affected, 2);
    })
    .await;
    txn.commit().await.expect("the transaction should commit");

    for id in ids {
        let audits = audits_of(&db, id).await;
        let update = audits
            .iter()
            .find(|audit| audit.action == "update")
            .expect("an audit row for the update");

        let before = update.before_content.as_ref().expect("a before");
        let after = update.after_content.as_ref().expect("an after");
        assert_eq!(before["name"], json!("Ada"));
        assert_eq!(after["name"], json!("Grace"));
        assert_eq!(before["age"], json!(36));
        assert_eq!(after["age"], json!(37));

        // The same reconcile as the hook path: a changed credential is marked
        // on both sides, and neither hash is stored.
        assert_eq!(
            before["password_hash"],
            json!(seaorm_auto_audit::REDACTED)
        );
        assert_eq!(after["password_hash"], json!(seaorm_auto_audit::REDACTED));
    }
}

#[tokio::test]
async fn delete_many_audits_every_row_it_removed() {
    let Some(db) = database().await else {
        return;
    };
    let ids = [Uuid::new_v4(), Uuid::new_v4()];
    let actor = Uuid::new_v4();

    let txn = db.begin().await.expect("the transaction should start");
    scope(Some(actor), async {
        audited_insert_many::<common::Entity, _, _>(&txn, users(ids))
            .await
            .expect("the seed insert should succeed");

        let filter = Condition::all().add(common::Column::Id.is_in(ids));
        let result = audited_delete_many::<common::Entity, _>(&txn, filter)
            .await
            .expect("the bulk delete should succeed");
        assert_eq!(result.rows_affected, 2);
    })
    .await;
    txn.commit().await.expect("the transaction should commit");

    for id in ids {
        let audits = audits_of(&db, id).await;
        let delete = audits
            .iter()
            .find(|audit| audit.action == "delete")
            .expect("an audit row for the delete");

        assert!(delete.after_content.is_none(), "a delete has no after");
        let before = delete.before_content.as_ref().expect("a before");
        assert_eq!(before["name"], json!("Ada"));
        assert!(
            before.get("password_hash").is_none(),
            "the credential is never stored: {before}"
        );
    }
}

#[tokio::test]
async fn delete_by_id_is_audited() {
    let Some(db) = database().await else {
        return;
    };
    let id = Uuid::new_v4();
    let actor = Uuid::new_v4();

    let txn = db.begin().await.expect("the transaction should start");
    scope(Some(actor), async {
        ActiveModel {
            id: Set(id),
            name: Set("Ada".to_owned()),
            email: Set(None),
            password_hash: Set(None),
            age: Set(36),
        }
        .insert(&txn)
        .await
        .expect("the seed insert should succeed");

        let result = audited_delete_by_id::<common::Entity, _, _>(&txn, id)
            .await
            .expect("the delete should succeed");
        assert_eq!(result.rows_affected, 1);
    })
    .await;
    txn.commit().await.expect("the transaction should commit");

    let audits = audits_of(&db, id).await;
    let delete = audits
        .iter()
        .find(|audit| audit.action == "delete")
        .expect("an audit row for the delete");

    assert_eq!(delete.updated_by_id, Some(actor));
    assert!(delete.after_content.is_none(), "a delete has no after");
    let before = delete.before_content.as_ref().expect("a before");
    assert_eq!(before["name"], json!("Ada"));
}
