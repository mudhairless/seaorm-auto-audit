//! Fixtures shared by the test files.
//!
//! The auditable table is a small `users` table with a `uuid` primary key, a
//! nullable `password_hash` (the credential column) and a couple of ordinary
//! columns to look at.

#![allow(dead_code)]

use std::collections::BTreeMap;

use sea_orm::entity::prelude::*;
use sea_orm::{Statement, Value};
use serde::{Deserialize, Serialize};

/// A user of the audited table.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "users")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    #[sea_orm(column_type = "Text")]
    pub name: String,
    #[sea_orm(column_type = "Text", nullable)]
    pub email: Option<String>,
    /// Credential material: never stored in an audit row.
    #[sea_orm(column_type = "Text", nullable)]
    pub password_hash: Option<String>,
    pub age: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

seaorm_auto_audit::impl_audit_hooks!(Entity, ActiveModel);

impl seaorm_auto_audit::Auditable for Entity {
    fn redacted_columns() -> &'static [&'static str] {
        &["password_hash"]
    }
}

/// A user row.
pub fn user(id: Uuid) -> Model {
    Model {
        id,
        name: "Ada".to_owned(),
        email: Some("ada@example.com".to_owned()),
        password_hash: Some("hash-before".to_owned()),
        age: 36,
    }
}

/// The values bound to an `INSERT` statement, keyed by the column they belong
/// to.
///
/// The column order of the insert follows the order of the fields, so the
/// values can be matched up by position.
pub fn insert_bindings(statement: &Statement) -> BTreeMap<String, Value> {
    let columns = insert_columns(&statement.sql);
    let values: Vec<Value> = statement
        .values
        .as_ref()
        .map(|values| values.0.clone())
        .unwrap_or_default();

    columns.into_iter().zip(values).collect()
}

/// The column names of an `INSERT` statement, in order.
pub fn insert_columns(sql: &str) -> Vec<String> {
    let start = sql.find('(').expect("insert statement without columns");
    let end = start
        + sql[start..]
            .find(')')
            .expect("insert statement without columns");

    sql[start + 1..end]
        .split(',')
        .map(|column| column.trim().trim_matches('"').to_owned())
        .collect()
}

/// The JSON held by a bound JSON value.
pub fn json_of(value: &Value) -> serde_json::Value {
    match value {
        Value::Json(Some(json)) => (**json).clone(),
        other => panic!("expected a JSON value, got {other:?}"),
    }
}
