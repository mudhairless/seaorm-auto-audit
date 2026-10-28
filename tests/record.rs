//! The audit record types an application builds by hand.
//!
//! Nothing inside the crate calls `AuditRecord::new` or parses an action
//! from text: the hooks build records as literals and write actions from
//! the enum. These tests pin the public surface that only applications
//! touch.

mod common;

use std::str::FromStr;

use common::{insert_bindings, json_of};
use sea_orm::{DbBackend, MockDatabase, MockExecResult};
use seaorm_auto_audit::{
    AuditAction, AuditId, AuditRecord, UnknownAction, write_record, write_records,
};
use serde_json::json;
use uuid::Uuid;

#[test]
fn a_new_record_carries_only_what_it_was_given() {
    let mut record = AuditRecord::new("users", 42_i64, AuditAction::Update);

    assert_eq!(record.auditable_table, "users");
    assert_eq!(record.auditable_id, AuditId::from(42_i64));
    assert_eq!(record.action, AuditAction::Update);
    assert_eq!(record.before_content, None);
    assert_eq!(record.after_content, None);
    assert_eq!(record.updated_by_id, None);
    assert_eq!(record.ip_address, None);
    assert_eq!(record.url, None);
    assert_eq!(record.user_agent, None);

    // The fields are public so the caller can fill in what it knows.
    record.before_content = Some(json!({ "name": "Ada" }));
    record.after_content = Some(json!({ "name": "Grace" }));
    record.updated_by_id = Some(AuditId::from(7_i64));

    let mut expected = AuditRecord::new("users", 42_i64, AuditAction::Update);
    expected.before_content = Some(json!({ "name": "Ada" }));
    expected.after_content = Some(json!({ "name": "Grace" }));
    expected.updated_by_id = Some(AuditId::from(7_i64));
    assert_eq!(record, expected);
    assert_ne!(
        record,
        AuditRecord::new("users", 42_i64, AuditAction::Create)
    );
}

#[test]
fn actions_round_trip_through_their_stored_text() {
    for (action, text) in [
        (AuditAction::Create, "create"),
        (AuditAction::Update, "update"),
        (AuditAction::Delete, "delete"),
        (AuditAction::Read, "read"),
    ] {
        assert_eq!(action.as_str(), text);
        assert_eq!(action.to_string(), text, "Display matches as_str");
        assert_eq!(AuditAction::from_str(text), Ok(action));
    }
}

#[test]
fn an_unknown_action_is_rejected_with_the_value_named() {
    let error = AuditAction::from_str("merge").expect_err("`merge` is not an action");

    assert_eq!(error, UnknownAction("merge".to_owned()));
    assert_eq!(error.to_string(), "`merge` is not a known audit action");

    // It is a real error type, usable with `?` and error chains.
    let _: &dyn std::error::Error = &error;
}

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
async fn write_record_stores_a_hand_built_record_as_given() {
    // The direct path's contract: no entity is involved, so nothing is
    // redacted on the caller's behalf. What is handed over is what lands
    // in the table. The hooks and the bulk wrappers redact because they
    // know the entity; a caller here applies the redaction itself.
    let db = MockDatabase::new(DbBackend::Postgres)
        .append_exec_results([MockExecResult {
            rows_affected: 1,
            ..Default::default()
        }])
        .into_connection();

    let mut record = AuditRecord::new("users", Uuid::new_v4(), AuditAction::Create);
    record.after_content = Some(json!({"name": "Ada", "password_hash": "a-hash"}));

    write_record(&db, record)
        .await
        .expect("the record should be written");

    let log = db.into_transaction_log();
    let bindings = insert_bindings(&audit_statement(&log));
    assert_eq!(
        json_of(&bindings["after_content"]),
        json!({"name": "Ada", "password_hash": "a-hash"}),
        "stored exactly as given: redaction on this path is the caller's"
    );
}

#[tokio::test]
async fn write_records_stores_hand_built_records_as_given_in_one_statement() {
    let db = MockDatabase::new(DbBackend::Postgres)
        .append_exec_results([MockExecResult {
            rows_affected: 2,
            ..Default::default()
        }])
        .into_connection();

    let records: Vec<AuditRecord> = (0..2)
        .map(|_| {
            let mut record = AuditRecord::new("users", Uuid::new_v4(), AuditAction::Create);
            record.after_content = Some(json!({"name": "Ada", "password_hash": "a-hash"}));
            record
        })
        .collect();

    write_records(&db, &records)
        .await
        .expect("the records should be written");

    let log = db.into_transaction_log();
    let audit_statements: Vec<_> = log
        .iter()
        .flat_map(|transaction| transaction.statements().iter())
        .filter(|statement| statement.sql.contains("audits"))
        .collect();
    assert_eq!(audit_statements.len(), 1, "one statement for the batch");

    let bindings = insert_bindings(&audit_statement(&log));
    assert_eq!(
        json_of(&bindings["after_content"]),
        json!({"name": "Ada", "password_hash": "a-hash"}),
        "stored exactly as given: redaction on this path is the caller's"
    );
}
