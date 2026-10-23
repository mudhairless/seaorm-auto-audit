//! The three write paths, against SeaORM's `MockDatabase`.
//!
//! The mock records every statement it executes, so the tests can check what
//! the hooks wrote, in which order, and with which bound values.

mod common;

use common::{ActiveModel, Entity, insert_bindings, json_of, user};
use seaorm_auto_audit::{REDACTED, after_save, before_save, scope};
use sea_orm::{
    ActiveModelTrait,
    ActiveValue::{NotSet, Set, Unchanged},
    DbBackend, DbErr, MockDatabase, MockExecResult, TransactionTrait, Value,
};
use serde_json::json;
use uuid::Uuid;

/// The audit statement of a transaction log: the last statement of the last
/// transaction.
fn audit_statement(log: &[sea_orm::Transaction]) -> sea_orm::Statement {
    let transaction = log.last().expect("no transaction was recorded");
    let statements = transaction.statements();
    let statement = statements.last().expect("no statement was recorded");
    assert!(
        statement.sql.contains("audits"),
        "expected an insert into audits, got: {}",
        statement.sql
    );
    statement.clone()
}

#[tokio::test]
async fn an_insert_writes_one_audit_row() {
    let id = Uuid::new_v4();
    let actor = Uuid::new_v4();
    let db = MockDatabase::new(DbBackend::Postgres)
        .append_query_results([[user(id)]])
        .append_exec_results([MockExecResult {
            rows_affected: 1,
            ..Default::default()
        }])
        .into_connection();

    scope(Some(actor), async {
        ActiveModel {
            id: Set(id),
            name: Set("Ada".to_owned()),
            email: Set(Some("ada@example.com".to_owned())),
            password_hash: Set(Some("hash-before".to_owned())),
            age: Set(36),
        }
        .insert(&db)
        .await
        .expect("the insert should succeed");
    })
    .await;

    let log = db.into_transaction_log();
    assert_eq!(log.len(), 2, "the write and the audit row");

    let bindings = insert_bindings(&audit_statement(&log));
    assert_eq!(
        bindings["auditable_table"],
        Value::String(Some("users".to_owned()))
    );
    assert_eq!(bindings["auditable_id"], Value::Uuid(Some(id)));
    assert_eq!(bindings["action"], Value::String(Some("create".to_owned())));
    assert_eq!(bindings["updated_by_id"], Value::Uuid(Some(actor)));

    // A create has no "before".
    assert_eq!(bindings["before_content"], Value::Json(None));

    // The "after" is the row the database returned, minus the credential.
    let after = json_of(&bindings["after_content"]);
    assert_eq!(after["name"], json!("Ada"));
    assert_eq!(after["age"], json!(36));
    assert!(after.get("password_hash").is_none());
}

#[tokio::test]
async fn an_update_records_the_before_and_after_state() {
    let id = Uuid::new_v4();
    let actor = Uuid::new_v4();
    let mut after = user(id);
    after.name = "Grace".to_owned();
    after.age = 37;

    let db = MockDatabase::new(DbBackend::Postgres)
        // the `SELECT` in `before_save`
        .append_query_results([[user(id)]])
        // the `UPDATE ... RETURNING`
        .append_query_results([[after.clone()]])
        .append_exec_results([MockExecResult {
            rows_affected: 1,
            ..Default::default()
        }])
        .into_connection();

    scope(Some(actor), async {
        ActiveModel {
            id: Unchanged(id),
            name: Set("Grace".to_owned()),
            email: NotSet,
            password_hash: NotSet,
            age: Set(37),
        }
        .update(&db)
        .await
        .expect("the update should succeed");
    })
    .await;

    let log = db.into_transaction_log();
    assert_eq!(log.len(), 3, "the read, the write and the audit row");

    let bindings = insert_bindings(&audit_statement(&log));
    assert_eq!(bindings["action"], Value::String(Some("update".to_owned())));
    assert_eq!(bindings["auditable_id"], Value::Uuid(Some(id)));
    assert_eq!(bindings["updated_by_id"], Value::Uuid(Some(actor)));

    let before = json_of(&bindings["before_content"]);
    let after = json_of(&bindings["after_content"]);
    assert_eq!(before["name"], json!("Ada"));
    assert_eq!(after["name"], json!("Grace"));
    assert_eq!(before["age"], json!(36));
    assert_eq!(after["age"], json!(37));
}

#[tokio::test]
async fn a_delete_records_the_state_the_row_had() {
    let id = Uuid::new_v4();
    let actor = Uuid::new_v4();

    let db = MockDatabase::new(DbBackend::Postgres)
        // the `SELECT` in `before_delete`
        .append_query_results([[user(id)]])
        .append_exec_results([
            MockExecResult {
                rows_affected: 1,
                ..Default::default()
            },
            MockExecResult {
                rows_affected: 1,
                ..Default::default()
            },
        ])
        .into_connection();

    scope(Some(actor), async {
        ActiveModel {
            id: Unchanged(id),
            name: NotSet,
            email: NotSet,
            password_hash: NotSet,
            age: NotSet,
        }
        .delete(&db)
        .await
        .expect("the delete should succeed");
    })
    .await;

    let log = db.into_transaction_log();
    assert_eq!(log.len(), 3, "the read, the delete and the audit row");

    let bindings = insert_bindings(&audit_statement(&log));
    assert_eq!(bindings["action"], Value::String(Some("delete".to_owned())));
    assert_eq!(bindings["auditable_id"], Value::Uuid(Some(id)));
    assert_eq!(bindings["updated_by_id"], Value::Uuid(Some(actor)));

    // A delete has no "after".
    assert_eq!(bindings["after_content"], Value::Json(None));

    let before = json_of(&bindings["before_content"]);
    assert_eq!(before["name"], json!("Ada"));
    assert!(before.get("password_hash").is_none());
}

#[tokio::test]
async fn the_audit_row_is_written_on_the_handle_of_the_write() {
    let id = Uuid::new_v4();
    let db = MockDatabase::new(DbBackend::Postgres)
        .append_query_results([[user(id)]])
        .append_exec_results([MockExecResult {
            rows_affected: 1,
            ..Default::default()
        }])
        .into_connection();

    let txn = db.begin().await.expect("the transaction should start");
    scope(Some(Uuid::new_v4()), async {
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

    let log = db.into_transaction_log();
    assert_eq!(log.len(), 1, "everything happened in one transaction");

    let statements = log[0].statements();
    let sql: Vec<&str> = statements
        .iter()
        .map(|statement| statement.sql.as_str())
        .collect();
    assert!(
        sql[0].starts_with("BEGIN"),
        "expected the transaction to open first, got: {}",
        sql[0]
    );
    assert!(
        sql[sql.len() - 1].starts_with("COMMIT"),
        "expected the transaction to close last, got: {}",
        sql[sql.len() - 1]
    );
    assert!(
        sql.iter().any(|statement| statement.contains("\"audits\"")),
        "the audit row belongs to the transaction: {sql:?}"
    );
}

#[tokio::test]
async fn a_write_outside_a_scope_fails_before_touching_the_database() {
    let id = Uuid::new_v4();
    let db = MockDatabase::new(DbBackend::Postgres)
        .append_query_results([[user(id)]])
        .into_connection();

    let result = ActiveModel {
        id: Set(id),
        name: Set("Ada".to_owned()),
        email: Set(None),
        password_hash: Set(None),
        age: Set(36),
    }
    .insert(&db)
    .await;

    let Err(DbErr::Custom(message)) = result else {
        panic!("expected the write to fail without a scope");
    };
    assert!(
        message.contains("no audit scope"),
        "expected a missing-scope error, got: {message}"
    );

    let log = db.into_transaction_log();
    assert!(log.is_empty(), "the write never ran");
}

#[tokio::test]
async fn a_machine_made_change_is_audited_without_an_actor() {
    let id = Uuid::new_v4();
    let db = MockDatabase::new(DbBackend::Postgres)
        .append_query_results([[user(id)]])
        .append_exec_results([MockExecResult {
            rows_affected: 1,
            ..Default::default()
        }])
        .into_connection();

    scope(None, async {
        ActiveModel {
            id: Set(id),
            name: Set("Ada".to_owned()),
            email: Set(None),
            password_hash: Set(None),
            age: Set(36),
        }
        .insert(&db)
        .await
        .expect("the insert should succeed");
    })
    .await;

    let log = db.into_transaction_log();
    let bindings = insert_bindings(&audit_statement(&log));

    // Nobody was behind the change, but the change is still audited.
    assert_eq!(bindings["updated_by_id"], Value::Uuid(None));
    assert_eq!(bindings["action"], Value::String(Some("create".to_owned())));
}

#[tokio::test]
async fn two_rows_of_one_scope_keep_their_own_before_state() {
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let mut second_after = user(second);
    second_after.name = "Grace".to_owned();

    let db = MockDatabase::new(DbBackend::Postgres)
        // first row: the `SELECT`, then the `UPDATE ... RETURNING`
        .append_query_results([[user(first)]])
        .append_query_results([[{
            let mut row = user(first);
            row.name = "Ada Lovelace".to_owned();
            row
        }]])
        // second row: the `SELECT`, then the `UPDATE ... RETURNING`
        .append_query_results([[user(second)]])
        .append_query_results([[second_after]])
        .append_exec_results([
            MockExecResult {
                rows_affected: 1,
                ..Default::default()
            },
            MockExecResult {
                rows_affected: 1,
                ..Default::default()
            },
        ])
        .into_connection();

    scope(Some(Uuid::new_v4()), async {
        ActiveModel {
            id: Unchanged(first),
            name: Set("Ada Lovelace".to_owned()),
            email: NotSet,
            password_hash: NotSet,
            age: NotSet,
        }
        .update(&db)
        .await
        .expect("the first update should succeed");

        ActiveModel {
            id: Unchanged(second),
            name: Set("Grace".to_owned()),
            email: NotSet,
            password_hash: NotSet,
            age: NotSet,
        }
        .update(&db)
        .await
        .expect("the second update should succeed");
    })
    .await;

    let log = db.into_transaction_log();
    let audit_rows: Vec<_> = log
        .iter()
        .flat_map(|transaction| transaction.statements().iter())
        .filter(|statement| statement.sql.contains("audits"))
        .map(insert_bindings)
        .collect();

    assert_eq!(audit_rows.len(), 2, "one audit row per write");

    let first_row = audit_rows
        .iter()
        .find(|bindings| bindings["auditable_id"] == Value::Uuid(Some(first)))
        .expect("an audit row for the first row");
    let second_row = audit_rows
        .iter()
        .find(|bindings| bindings["auditable_id"] == Value::Uuid(Some(second)))
        .expect("an audit row for the second row");

    // Each audit row holds its own row's before-state.
    assert_eq!(
        json_of(&first_row["before_content"])["name"],
        json!("Ada"),
        "the first row's before-state"
    );
    assert_eq!(
        json_of(&second_row["before_content"])["name"],
        json!("Ada"),
        "the second row's before-state"
    );
    assert_eq!(
        json_of(&second_row["after_content"])["name"],
        json!("Grace")
    );
}

#[tokio::test]
async fn a_credential_change_is_marked_but_never_stored() {
    let id = Uuid::new_v4();
    let mut after = user(id);
    after.password_hash = Some("hash-after".to_owned());

    let db = MockDatabase::new(DbBackend::Postgres)
        .append_query_results([[user(id)]])
        .append_query_results([[after]])
        .append_exec_results([MockExecResult {
            rows_affected: 1,
            ..Default::default()
        }])
        .into_connection();

    scope(Some(Uuid::new_v4()), async {
        ActiveModel {
            id: Unchanged(id),
            name: NotSet,
            email: NotSet,
            password_hash: Set(Some("hash-after".to_owned())),
            age: NotSet,
        }
        .update(&db)
        .await
        .expect("the update should succeed");
    })
    .await;

    let log = db.into_transaction_log();
    let bindings = insert_bindings(&audit_statement(&log));

    let before = json_of(&bindings["before_content"]);
    let after = json_of(&bindings["after_content"]);

    // The column is present on both sides, marked, and holding no hash.
    assert_eq!(before["password_hash"], json!(REDACTED));
    assert_eq!(after["password_hash"], json!(REDACTED));

    // The raw hashes are nowhere in the audit row.
    let row = format!("{before}{after}");
    assert!(!row.contains("hash-before"), "the old hash leaked");
    assert!(!row.contains("hash-after"), "the new hash leaked");
}

#[tokio::test]
async fn an_unchanged_credential_is_dropped_from_the_audit_row() {
    let id = Uuid::new_v4();
    let db = MockDatabase::new(DbBackend::Postgres)
        .append_query_results([[user(id)]])
        .append_query_results([[user(id)]])
        .append_exec_results([MockExecResult {
            rows_affected: 1,
            ..Default::default()
        }])
        .into_connection();

    scope(Some(Uuid::new_v4()), async {
        ActiveModel {
            id: Unchanged(id),
            name: Set("Grace".to_owned()),
            email: NotSet,
            password_hash: NotSet,
            age: NotSet,
        }
        .update(&db)
        .await
        .expect("the update should succeed");
    })
    .await;

    let log = db.into_transaction_log();
    let bindings = insert_bindings(&audit_statement(&log));

    // The credential did not move, so it says nothing: it is dropped.
    assert!(
        json_of(&bindings["before_content"])
            .get("password_hash")
            .is_none()
    );
    assert!(
        json_of(&bindings["after_content"])
            .get("password_hash")
            .is_none()
    );
}

#[tokio::test]
async fn a_null_credential_becoming_a_hash_is_a_change() {
    let id = Uuid::new_v4();
    let mut before = user(id);
    before.password_hash = None;
    let mut after = before.clone();
    after.password_hash = Some("hash-after".to_owned());

    let db = MockDatabase::new(DbBackend::Postgres)
        .append_query_results([[before]])
        .append_query_results([[after]])
        .append_exec_results([MockExecResult {
            rows_affected: 1,
            ..Default::default()
        }])
        .into_connection();

    scope(Some(Uuid::new_v4()), async {
        ActiveModel {
            id: Unchanged(id),
            name: NotSet,
            email: NotSet,
            password_hash: Set(Some("hash-after".to_owned())),
            age: NotSet,
        }
        .update(&db)
        .await
        .expect("the update should succeed");
    })
    .await;

    let log = db.into_transaction_log();
    let bindings = insert_bindings(&audit_statement(&log));

    assert_eq!(
        json_of(&bindings["before_content"])["password_hash"],
        json!(REDACTED),
        "None to Some is a change"
    );
    assert_eq!(
        json_of(&bindings["after_content"])["password_hash"],
        json!(REDACTED)
    );
}

#[tokio::test]
async fn a_bulk_write_outside_a_scope_fails_before_touching_the_database() {
    let db = MockDatabase::new(DbBackend::Postgres).into_connection();

    let error =
        seaorm_auto_audit::delete_many::<Entity, _>(&db, sea_orm::Condition::all())
            .await
            .expect_err("a bulk write outside a scope must fail");

    assert!(error.to_string().contains("no audit scope"), "{error}");
    let log = db.into_transaction_log();
    assert!(log.is_empty(), "no statement should have run");
}

/// The `UPDATE` half of a write, for the interleaving test.
fn update(id: Uuid, name: &str) -> ActiveModel {
    ActiveModel {
        id: Unchanged(id),
        name: Set(name.to_owned()),
        email: NotSet,
        password_hash: NotSet,
        age: NotSet,
    }
}

#[tokio::test]
async fn two_interleaved_writes_to_the_same_row_do_not_share_a_before_state() {
    // Two updates of one row interleaved in a single scope: both `before_save`
    // calls run before either `after_save` (two concurrent futures on one
    // task). The stash is keyed by table and primary key, so the second
    // snapshot overwrites the first. The first `after_save` then consumes the
    // second write's snapshot, and the second fails loudly.
    let id = Uuid::new_v4();
    let first_seen = user(id);
    let mut second_seen = user(id);
    second_seen.name = "Grace".to_owned();

    let db = MockDatabase::new(DbBackend::Postgres)
        // the `SELECT` of the first `before_save`
        .append_query_results([[first_seen]])
        // the `SELECT` of the second `before_save`
        .append_query_results([[second_seen]])
        // the audit row of the first `after_save`
        .append_exec_results([MockExecResult {
            rows_affected: 1,
            ..Default::default()
        }])
        .into_connection();

    scope(Some(Uuid::new_v4()), async {
        before_save::<Entity, _>(update(id, "Ada Lovelace"), &db, false)
            .await
            .expect("the first write reads its before state");
        before_save::<Entity, _>(update(id, "Grace Hopper"), &db, false)
            .await
            .expect("the second write reads its before state");

        let mut first_after = user(id);
        first_after.name = "Ada Lovelace".to_owned();
        after_save::<Entity, _>(first_after, &db, false)
            .await
            .expect("the first `after_save` consumes the parked snapshot");

        let mut second_after = user(id);
        second_after.name = "Grace Hopper".to_owned();
        let error = after_save::<Entity, _>(second_after, &db, false)
            .await
            .expect_err("the second `after_save` has no snapshot left");
        assert!(
            error.to_string().contains("no before-state"),
            "expected the loud no-before-state error, got: {error}"
        );
    })
    .await;

    let log = db.into_transaction_log();
    assert_eq!(log.len(), 3, "two reads and one audit row");

    // The one audit row that was written carries the later snapshot, not the
    // snapshot of the write it describes. Interleaving writes to one row in
    // one scope is the documented limit; sequential writes are unaffected.
    let bindings = insert_bindings(&audit_statement(&log));
    assert_eq!(json_of(&bindings["before_content"])["name"], json!("Grace"));
    assert_eq!(
        json_of(&bindings["after_content"])["name"],
        json!("Ada Lovelace")
    );
}
