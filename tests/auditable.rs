//! The `Auditable` defaults, for the shapes the shared fixtures do not have.
//!
//! Every fixture entity in `tests/common` has a single-column primary key,
//! so the default `audit_id` always succeeds there. A composite key is the
//! one shape that makes the default stop and name the override instead.

use sea_orm::entity::prelude::*;
use seaorm_auto_audit::Auditable;
use serde::Serialize;

/// A table whose primary key spans two columns.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize)]
#[sea_orm(table_name = "enrollments")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub student_id: i64,
    #[sea_orm(primary_key, auto_increment = false)]
    pub course_id: i64,
    #[sea_orm(column_type = "Text", nullable)]
    pub grade: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl sea_orm::ActiveModelBehavior for ActiveModel {}

impl Auditable for Entity {}

#[test]
fn a_composite_primary_key_names_the_override() {
    let model = Model {
        student_id: 7,
        course_id: 42,
        grade: None,
    };

    let error = <Entity as Auditable>::audit_id(&model)
        .expect_err("a composite key has no single identifier");

    let message = error.to_string();
    assert!(message.contains("composite primary key"), "{message}");
    assert!(message.contains("enrollments"), "{message}");
    assert!(
        message.contains("override `Auditable::audit_id`"),
        "{message}"
    );
}
