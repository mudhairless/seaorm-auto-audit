//! Excluded columns: left out of audit records entirely, on every write
//! path, against SeaORM's `MockDatabase`.
//!
//! The fixture is a `sessions` table whose `last_seen_at` changes on every
//! write and means nothing in an audit trail: the textbook excluded column.

mod common;

use common::{insert_bindings, json_of};
use sea_orm::entity::prelude::*;
use sea_orm::{
    ActiveModelTrait,
    ActiveValue::{NotSet, Set, Unchanged},
    DbBackend, MockDatabase, MockExecResult,
};
use seaorm_auto_audit::scope;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "sessions")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    #[sea_orm(column_type = "Text")]
    pub label: String,
    pub last_seen_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

seaorm_auto_audit::impl_audit_hooks!(Entity, ActiveModel);

impl seaorm_auto_audit::Auditable for Entity {
    fn excluded_columns() -> &'static [&'static str] {
        &["last_seen_at"]
    }
}

fn session(id: Uuid) -> Model {
    Model {
        id,
        label: "laptop".to_owned(),
        last_seen_at: 100,
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

fn audit_writes(log: &[sea_orm::Transaction]) -> usize {
    log.iter()
        .flat_map(|transaction| transaction.statements().iter())
        .filter(|statement| statement.sql.contains("audits"))
        .count()
}

#[tokio::test]
async fn an_insert_omits_the_excluded_column() {
    let id = Uuid::new_v4();
    let db = MockDatabase::new(DbBackend::Postgres)
        .append_query_results([[session(id)]])
        .append_exec_results([MockExecResult {
            rows_affected: 1,
            ..Default::default()
        }])
        .into_connection();

    scope(None, async {
        ActiveModel {
            id: Set(id),
            label: Set("laptop".to_owned()),
            last_seen_at: Set(100),
        }
        .insert(&db)
        .await
        .expect("the insert should succeed");
    })
    .await;

    let log = db.into_transaction_log();
    let bindings = insert_bindings(&audit_statement(&log));
    let after = json_of(&bindings["after_content"]);
    assert_eq!(after["label"], json!("laptop"));
    assert!(
        after.get("last_seen_at").is_none(),
        "the excluded column is absent, not blanked: {after}"
    );
}

#[tokio::test]
async fn an_update_touching_only_an_excluded_column_writes_no_audit_row() {
    let id = Uuid::new_v4();
    let mut touched = session(id);
    touched.last_seen_at = 200;

    let db = MockDatabase::new(DbBackend::Postgres)
        .append_query_results([[session(id)]])
        .append_query_results([[touched]])
        .into_connection();

    scope(None, async {
        ActiveModel {
            id: Unchanged(id),
            label: NotSet,
            last_seen_at: Set(200),
        }
        .update(&db)
        .await
        .expect("the update should succeed");
    })
    .await;

    let log = db.into_transaction_log();
    assert_eq!(audit_writes(&log), 0, "the write itself, and no audit row");
}

#[tokio::test]
async fn an_update_omits_the_excluded_column_from_its_pair() {
    let id = Uuid::new_v4();
    let mut renamed = session(id);
    renamed.label = "tablet".to_owned();
    renamed.last_seen_at = 200;

    let db = MockDatabase::new(DbBackend::Postgres)
        .append_query_results([[session(id)]])
        .append_query_results([[renamed]])
        .append_exec_results([MockExecResult {
            rows_affected: 1,
            ..Default::default()
        }])
        .into_connection();

    scope(None, async {
        ActiveModel {
            id: Unchanged(id),
            label: Set("tablet".to_owned()),
            last_seen_at: Set(200),
        }
        .update(&db)
        .await
        .expect("the update should succeed");
    })
    .await;

    let log = db.into_transaction_log();
    let bindings = insert_bindings(&audit_statement(&log));
    assert_eq!(
        json_of(&bindings["before_content"]),
        json!({"label": "laptop"}),
        "only the recorded change is in the pair"
    );
    assert_eq!(
        json_of(&bindings["after_content"]),
        json!({"label": "tablet"})
    );
}

#[tokio::test]
async fn a_delete_omits_the_excluded_column() {
    let id = Uuid::new_v4();
    let db = MockDatabase::new(DbBackend::Postgres)
        .append_query_results([[session(id)]])
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

    scope(None, async {
        session(id)
            .delete(&db)
            .await
            .expect("the delete should succeed");
    })
    .await;

    let log = db.into_transaction_log();
    let bindings = insert_bindings(&audit_statement(&log));
    let before = json_of(&bindings["before_content"]);
    assert_eq!(before["label"], json!("laptop"));
    assert!(
        before.get("last_seen_at").is_none(),
        "the excluded column is absent, not blanked: {before}"
    );
}

#[tokio::test]
async fn insert_many_omits_the_excluded_column() {
    let id = Uuid::new_v4();
    let db = MockDatabase::new(DbBackend::Postgres)
        .append_query_results([[session(id)], [session(id)]])
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

    scope(None, async {
        seaorm_auto_audit::insert_many::<Entity, _, _>(
            &db,
            [ActiveModel {
                id: Set(id),
                label: Set("laptop".to_owned()),
                last_seen_at: Set(100),
            }],
        )
        .await
        .expect("the bulk insert should succeed");
    })
    .await;

    let log = db.into_transaction_log();
    let bindings = insert_bindings(&audit_statement(&log));
    let after = json_of(&bindings["after_content"]);
    assert_eq!(after["label"], json!("laptop"));
    assert!(
        after.get("last_seen_at").is_none(),
        "the excluded column is absent, not blanked: {after}"
    );
}

#[tokio::test]
async fn update_many_touching_only_an_excluded_column_writes_no_audit_row() {
    let id = Uuid::new_v4();
    let mut touched = session(id);
    touched.last_seen_at = 200;

    let db = MockDatabase::new(DbBackend::Postgres)
        .append_query_results([[session(id)]])
        .append_query_results([[touched]])
        .append_exec_results([MockExecResult {
            rows_affected: 1,
            ..Default::default()
        }])
        .into_connection();

    scope(None, async {
        seaorm_auto_audit::update_many::<Entity, _>(
            &db,
            sea_orm::Condition::all(),
            ActiveModel {
                id: NotSet,
                label: NotSet,
                last_seen_at: Set(200),
            },
        )
        .await
        .expect("the bulk update should succeed");
    })
    .await;

    let log = db.into_transaction_log();
    assert_eq!(audit_writes(&log), 0, "no row changed in any recorded way");
}

#[tokio::test]
async fn delete_many_omits_the_excluded_column() {
    let id = Uuid::new_v4();
    let db = MockDatabase::new(DbBackend::Postgres)
        .append_query_results([[session(id)]])
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

    scope(None, async {
        seaorm_auto_audit::delete_many::<Entity, _>(&db, sea_orm::Condition::all())
            .await
            .expect("the bulk delete should succeed");
    })
    .await;

    let log = db.into_transaction_log();
    let bindings = insert_bindings(&audit_statement(&log));
    let before = json_of(&bindings["before_content"]);
    assert_eq!(before["label"], json!("laptop"));
    assert!(
        before.get("last_seen_at").is_none(),
        "the excluded column is absent, not blanked: {before}"
    );
}
