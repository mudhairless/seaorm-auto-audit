//! Column names, not field names: snapshots are keyed by database column
//! name, and `redacted_columns` / `excluded_columns` are matched against
//! those names.
//!
//! The fixture's column names deliberately diverge from its Rust field
//! names and its serde names: `password_hash` is stored in `pwd_hash`,
//! and `api_key` is stored in `api_token` and serialized as `token`.
//! Before the fix, snapshots were built with serde, so the keys were the
//! serde names and listing the column name silently redacted nothing.

mod common;

use common::{insert_bindings, json_of};
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveModelTrait, ActiveValue::Set, DbBackend, MockDatabase, MockExecResult};
use seaorm_auto_audit::scope;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "accounts")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub name: String,
    #[sea_orm(column_name = "pwd_hash")]
    pub password_hash: Option<String>,
    #[sea_orm(column_name = "api_token")]
    #[serde(rename = "token")]
    pub api_key: Option<String>,
    #[sea_orm(column_name = "homepage_url")]
    pub homepage: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

seaorm_auto_audit::impl_audit_hooks!(Entity, ActiveModel);

impl seaorm_auto_audit::Auditable for Entity {
    fn redacted_columns() -> &'static [&'static str] {
        &["pwd_hash", "api_token"]
    }
}

fn account(id: Uuid) -> Model {
    Model {
        id,
        name: "Ada".to_owned(),
        password_hash: Some("secret-hash".to_owned()),
        api_key: Some("secret-token".to_owned()),
        homepage: Some("https://example.com".to_owned()),
    }
}

/// The audit statement of a transaction log: the last statement of the
/// last transaction.
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
async fn snapshots_are_keyed_by_column_name_and_redaction_matches_it() {
    let id = Uuid::new_v4();
    let db = MockDatabase::new(DbBackend::Postgres)
        .append_query_results([[account(id)]])
        .append_exec_results([MockExecResult {
            rows_affected: 1,
            ..Default::default()
        }])
        .into_connection();

    scope(None, async {
        ActiveModel {
            id: Set(id),
            name: Set("Ada".to_owned()),
            password_hash: Set(Some("secret-hash".to_owned())),
            api_key: Set(Some("secret-token".to_owned())),
            homepage: Set(Some("https://example.com".to_owned())),
        }
        .insert(&db)
        .await
        .expect("the insert should succeed");
    })
    .await;

    let log = db.into_transaction_log();
    let bindings = insert_bindings(&audit_statement(&log));
    let after = json_of(&bindings["after_content"]);

    // The keys are the database column names, not the field names and
    // not the serde names: the unredacted renamed column shows up
    // under its column name only.
    let keys: Vec<&str> = after
        .as_object()
        .expect("a snapshot object")
        .keys()
        .map(String::as_str)
        .collect();
    assert!(keys.contains(&"homepage_url"), "keys: {keys:?}");
    assert!(!keys.contains(&"homepage"), "keys: {keys:?}");

    // Both credentials were matched by their column names and dropped
    // (a create drops redacted columns outright), so neither secret
    // is in the snapshot in any form. With serde keys, the lists would
    // have matched nothing and both secrets would be stored.
    assert!(!keys.contains(&"pwd_hash"), "keys: {keys:?}");
    assert!(!keys.contains(&"api_token"), "keys: {keys:?}");
    let stored = after.to_string();
    assert!(!stored.contains("secret-hash"), "stored: {stored}");
    assert!(!stored.contains("secret-token"), "stored: {stored}");
}
