//! Identifiers that are not uuids: an entity with an integer primary key, and
//! the `AuditId` conversions the crate relies on.

mod common;

use common::{insert_bindings, json_of};
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveModelTrait, ActiveValue::Set, DbBackend, MockDatabase, MockExecResult, Value};
use seaorm_auto_audit::{AuditId, scope};
use serde::{Deserialize, Serialize};

/// A table whose primary key is an `i64`, to prove the auditable id does not
/// have to be a uuid.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "widgets")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: i64,
    pub name: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

seaorm_auto_audit::impl_audit_hooks!(Entity, ActiveModel);

impl seaorm_auto_audit::Auditable for Entity {}

fn widget(id: i64) -> Model {
    Model {
        id,
        name: "Sprocket".to_owned(),
    }
}

mod gadgets {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    /// A table whose primary key is a `u64`, which SeaORM exposes as
    /// `Value::BigUnsigned` rather than `Value::BigInt`.
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "gadgets")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: u64,
        pub name: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    seaorm_auto_audit::impl_audit_hooks!(Entity, ActiveModel);

    impl seaorm_auto_audit::Auditable for Entity {}
}

fn gadget(id: u64) -> gadgets::Model {
    gadgets::Model {
        id,
        name: "Cog".to_owned(),
    }
}

#[tokio::test]
async fn an_integer_key_is_stored_as_text_in_the_audit_row() {
    let id = 7_i64;
    let actor = 42_i64;
    let db = MockDatabase::new(DbBackend::Postgres)
        .append_query_results([[widget(id)]])
        .append_exec_results([MockExecResult {
            rows_affected: 1,
            ..Default::default()
        }])
        .into_connection();

    scope(Some(actor.into()), async {
        ActiveModel {
            id: Set(id),
            name: Set("Sprocket".to_owned()),
        }
        .insert(&db)
        .await
        .expect("the insert should succeed");
    })
    .await;

    let log = db.into_transaction_log();
    let statement = log.last().unwrap().statements().last().unwrap();
    let bindings = insert_bindings(statement);

    assert_eq!(
        bindings["auditable_id"],
        Value::String(Some("7".to_owned())),
        "the integer key is stored in its text form"
    );
    assert_eq!(
        bindings["updated_by_id"],
        Value::String(Some("42".to_owned())),
        "the actor's integer id is stored in its text form"
    );

    let after = json_of(&bindings["after_content"]);
    assert_eq!(after["id"], serde_json::json!(7));
    assert_eq!(after["name"], serde_json::json!("Sprocket"));
}

#[tokio::test]
async fn a_u64_key_is_stored_as_text_in_the_audit_row() {
    let id = 7_u64;
    let db = MockDatabase::new(DbBackend::Postgres)
        .append_query_results([[gadget(id)]])
        .append_exec_results([MockExecResult {
            rows_affected: 1,
            ..Default::default()
        }])
        .into_connection();

    scope(None, async {
        gadgets::ActiveModel {
            id: Set(id),
            name: Set("Cog".to_owned()),
        }
        .insert(&db)
        .await
        .expect("the insert should succeed");
    })
    .await;

    let log = db.into_transaction_log();
    let statement = log.last().unwrap().statements().last().unwrap();
    let bindings = insert_bindings(statement);

    assert_eq!(
        bindings["auditable_id"],
        Value::String(Some("7".to_owned())),
        "the u64 key is stored in its text form"
    );
    assert_eq!(
        bindings["auditable_table"],
        Value::String(Some("gadgets".to_owned()))
    );

    let after = json_of(&bindings["after_content"]);
    assert_eq!(after["id"], serde_json::json!(7));
    assert_eq!(after["name"], serde_json::json!("Cog"));
}

#[tokio::test]
async fn audit_row_ids_are_uuid_v7() {
    let id = 7_i64;
    let db = MockDatabase::new(DbBackend::Postgres)
        .append_query_results([[widget(id)]])
        .append_exec_results([MockExecResult {
            rows_affected: 1,
            ..Default::default()
        }])
        .into_connection();

    scope(None, async {
        ActiveModel {
            id: Set(id),
            name: Set("Sprocket".to_owned()),
        }
        .insert(&db)
        .await
        .expect("the insert should succeed");
    })
    .await;

    let log = db.into_transaction_log();
    let statement = log.last().unwrap().statements().last().unwrap();
    let bindings = insert_bindings(statement);

    let Value::Uuid(Some(audit_id)) = bindings["id"] else {
        panic!(
            "the audit row id should be a uuid, got {:?}",
            bindings["id"]
        );
    };
    assert_eq!(audit_id.get_version_num(), 7, "audit row ids are UUIDv7");
}

#[test]
fn audit_ids_convert_from_the_common_key_types() {
    let uuid = uuid::Uuid::now_v7();

    assert_eq!(AuditId::from(uuid).as_str(), uuid.to_string());
    assert_eq!(AuditId::from(7_i64).as_str(), "7");
    assert_eq!(AuditId::from(7_u64).as_str(), "7");
    assert_eq!(AuditId::from(7_i32).as_str(), "7");
    assert_eq!(AuditId::from("widget-7").as_str(), "widget-7");
    assert_eq!(AuditId::from("widget-7".to_owned()).as_str(), "widget-7");

    let from_value = AuditId::from_value(&Value::BigInt(Some(7))).unwrap();
    assert_eq!(from_value.as_str(), "7");
    let from_value = AuditId::from_value(&Value::TinyUnsigned(Some(7))).unwrap();
    assert_eq!(from_value.as_str(), "7");
    let from_value = AuditId::from_value(&Value::SmallUnsigned(Some(7))).unwrap();
    assert_eq!(from_value.as_str(), "7");
    let from_value = AuditId::from_value(&Value::Unsigned(Some(7))).unwrap();
    assert_eq!(from_value.as_str(), "7");
    let from_value = AuditId::from_value(&Value::BigUnsigned(Some(7))).unwrap();
    assert_eq!(from_value.as_str(), "7");
    let from_value = AuditId::from_value(&Value::String(Some("widget-7".to_owned()))).unwrap();
    assert_eq!(from_value.as_str(), "widget-7");
    let from_value = AuditId::from_value(&Value::Uuid(Some(uuid))).unwrap();
    assert_eq!(from_value.as_str(), uuid.to_string());
}

#[test]
fn an_unsupported_value_is_rejected_with_an_explanation() {
    let error =
        AuditId::from_value(&Value::Bool(Some(true))).expect_err("a boolean is not an identifier");
    assert!(error.to_string().contains("override `Auditable::audit_id`"));
}
