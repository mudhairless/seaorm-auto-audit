//! Read records, against SeaORM's `MockDatabase`.
//!
//! The fixture is a `charts` table: `ssn` is redacted and `internal_note`
//! is excluded, so the fetched-payload stripping is exercised against both
//! lists.

mod common;

use common::{insert_bindings, json_of};
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveModelBehavior, DbBackend, MockDatabase, MockExecResult};
use seaorm_auto_audit::{record_read, record_reads, scope};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "charts")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    #[sea_orm(column_type = "Text")]
    pub name: String,
    #[sea_orm(column_type = "Text")]
    pub diagnosis: String,
    #[sea_orm(column_type = "Text")]
    pub ssn: String,
    #[sea_orm(column_type = "Text")]
    pub internal_note: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

impl seaorm_auto_audit::Auditable for Entity {
    fn redacted_columns() -> &'static [&'static str] {
        &["ssn"]
    }

    fn excluded_columns() -> &'static [&'static str] {
        &["internal_note"]
    }
}

fn chart(id: Uuid) -> Model {
    Model {
        id,
        name: "Ada".to_owned(),
        diagnosis: "sprained ankle".to_owned(),
        ssn: "123-45-6789".to_owned(),
        internal_note: "vip".to_owned(),
    }
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

fn mock_db() -> sea_orm::DatabaseConnection {
    MockDatabase::new(DbBackend::Postgres)
        .append_exec_results([MockExecResult {
            rows_affected: 1,
            ..Default::default()
        }])
        .into_connection()
}

#[tokio::test]
async fn a_read_is_recorded_with_its_comment_and_fetched_columns() {
    let id = Uuid::new_v4();
    let actor = Uuid::new_v4();
    let db = mock_db();

    scope(Some(actor.into()), async {
        record_read::<Entity, _>(
            &db,
            &chart(id),
            Some("opened for viewing"),
            Some(json!({
                "name": "Ada",
                "diagnosis": "sprained ankle",
                "ssn": "123-45-6789",
                "internal_note": "vip",
            })),
        )
        .await
        .expect("the read should be recorded");
    })
    .await;

    let log = db.into_transaction_log();
    let bindings = insert_bindings(&audit_statement(&log));
    assert_eq!(
        bindings["auditable_table"],
        sea_orm::Value::String(Some("charts".to_owned()))
    );
    assert_eq!(
        bindings["auditable_id"],
        sea_orm::Value::String(Some(id.to_string()))
    );
    assert_eq!(
        bindings["action"],
        sea_orm::Value::String(Some("read".to_owned()))
    );
    assert_eq!(
        bindings["updated_by_id"],
        sea_orm::Value::String(Some(actor.to_string()))
    );
    assert_eq!(
        bindings["comment"],
        sea_orm::Value::String(Some("opened for viewing".to_owned()))
    );
    assert_eq!(bindings["before_content"], sea_orm::Value::Json(None));
    assert_eq!(
        json_of(&bindings["after_content"]),
        json!({"name": "Ada", "diagnosis": "sprained ankle"}),
        "the fetched columns, minus the redacted and excluded keys"
    );
}

#[tokio::test]
async fn a_read_without_comment_or_fetched_still_records_the_event() {
    let id = Uuid::new_v4();
    let db = mock_db();

    scope(None, async {
        record_read::<Entity, _>(&db, &chart(id), None, None)
            .await
            .expect("the read should be recorded");
    })
    .await;

    let log = db.into_transaction_log();
    let bindings = insert_bindings(&audit_statement(&log));
    assert_eq!(
        bindings["action"],
        sea_orm::Value::String(Some("read".to_owned()))
    );
    assert_eq!(bindings["comment"], sea_orm::Value::String(None));
    assert_eq!(bindings["after_content"], sea_orm::Value::Json(None));
}

#[tokio::test]
async fn a_read_outside_a_scope_fails_and_writes_nothing() {
    let id = Uuid::new_v4();
    let db = mock_db();

    let error = record_read::<Entity, _>(&db, &chart(id), None, None)
        .await
        .expect_err("a read outside a scope must fail");

    assert!(error.to_string().contains("no audit scope"), "{error}");
    let log = db.into_transaction_log();
    assert!(log.is_empty(), "no statement should have run");
}

#[tokio::test]
async fn record_reads_records_a_page_in_one_statement() {
    let ids: Vec<Uuid> = (0..3).map(|_| Uuid::new_v4()).collect();
    let models: Vec<Model> = ids.iter().map(|id| chart(*id)).collect();
    let db = mock_db();

    scope(None, async {
        record_reads::<Entity, _>(
            &db,
            &models,
            Some("search results"),
            Some(json!({"name": null, "ssn": null})),
        )
        .await
        .expect("the reads should be recorded");
    })
    .await;

    let log = db.into_transaction_log();
    let audit_statements: Vec<_> = log
        .iter()
        .flat_map(|transaction| transaction.statements().iter())
        .filter(|statement| statement.sql.contains("audits"))
        .collect();
    assert_eq!(
        audit_statements.len(),
        1,
        "one statement for the whole page"
    );

    let bindings = insert_bindings(&audit_statement(&log));
    assert_eq!(
        bindings["comment"],
        sea_orm::Value::String(Some("search results".to_owned()))
    );
    assert_eq!(
        json_of(&bindings["after_content"]),
        json!({"name": null}),
        "the exposed columns, with the redacted key stripped"
    );
}

#[tokio::test]
async fn a_fetched_value_that_is_not_an_object_is_rejected() {
    let id = Uuid::new_v4();
    let db = mock_db();

    let result = scope(None, async {
        record_read::<Entity, _>(&db, &chart(id), None, Some(json!(["name", "diagnosis"]))).await
    })
    .await;

    let error = result.expect_err("a non-object fetched value must be refused");
    assert!(
        error
            .to_string()
            .contains("`fetched` must be a JSON object"),
        "{error}"
    );
    let log = db.into_transaction_log();
    assert!(log.is_empty(), "no statement should have run");
}
